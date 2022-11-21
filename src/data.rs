//! Persistence to DHT and RocksDB.

use std::{
	sync::Arc,
	time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use avail_subxt::primitives::Header as DaHeader;
use codec::{Decode, Encode};
use futures::{future::join_all, stream};
use kate_recovery::{data::Cell, matrix::Position};
use libp2p::kad::{record::Key, Quorum, Record};
use rocksdb::DB;
use tracing::{debug, trace};

use crate::{
	consts::{APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF},
	network::Client,
};

async fn fetch_cell_from_dht(
	network_client: &Client,
	block_number: u32,
	position: &Position,
) -> Result<Cell> {
	let reference = position.reference(block_number);
	let record_key = Key::from(reference.as_bytes().to_vec());

	trace!("Getting DHT record for reference {}", reference);
	// For now, we take only the first record from the list
	network_client
		.get_kad_record(record_key, Quorum::One)
		.await
		.and_then(|peer_records| {
			peer_records
				.get(0)
				.cloned()
				.context("Peer record not found")
		})
		.and_then(|peer_record| {
			peer_record
				.record
				.value
				.try_into()
				.map_err(|_| anyhow::anyhow!("Cannot convert record into 80 bytes"))
		})
		.map(|record| Cell {
			position: position.clone(),
			content: record,
		})
}

struct DHTCell(Cell);

impl DHTCell {
	fn reference(&self, block: u32) -> String {
		self.0.reference(block)
	}

	fn dht_record(&self, block: u32, ttl: u64) -> Record {
		Record {
			key: self.0.reference(block).as_bytes().to_vec().into(),
			value: self.0.content.to_vec(),
			publisher: None,
			expires: Instant::now().checked_add(Duration::from_secs(ttl)),
		}
	}
}

/// Inserts cells into the DHT.
/// There is no rollback, and errors will be logged and skipped,
/// which means that we cannot rely on error logs as alert mechanism.
///
/// # Arguments
///
/// * `network_client` - Reference to a libp2p custom network client
/// * `block` - Block number
/// * `cells` - Matrix cells to store into DHT
/// * `dht_parallelization_limit` - Number of cells to fetch in parallel
/// * `ttl` - Cell time to live in DHT (in seconds)
pub async fn insert_into_dht(
	network_client: &Client,
	block: u32,
	cells: Vec<Cell>,
	dht_parallelization_limit: usize,
	ttl: u64,
) {
	let cells: Vec<_> = cells.into_iter().map(DHTCell).collect::<Vec<_>>();
	let cell_tuples = cells.iter().map(move |b| (b, network_client.clone()));

	futures::StreamExt::for_each_concurrent(
		stream::iter(cell_tuples),
		dht_parallelization_limit,
		|(cell, network_client)| async move {
			let reference = cell.reference(block);
			if let Err(error) = network_client
				.put_kad_record(cell.dht_record(block, ttl), Quorum::One)
				.await
			{
				debug!("Fail to put record for cell {reference} to DHT: {error}");
			}
		},
	)
	.await;
}

/// Fetches cells from DHT.
/// Returns fetched cells and unfetched positions (so we can try RPC fetch).
///
/// # Arguments
///
/// * `network_client` - Reference to a libp2p custom network client
/// * `block_number` - Block number
/// * `positions` - Cell positions to fetch
/// * `dht_parallelization_limit` - Number of cells to fetch in parallel
pub async fn fetch_cells_from_dht(
	network_client: &Client,
	block_number: u32,
	positions: &Vec<Position>,
	dht_parallelization_limit: usize,
) -> Result<(Vec<Cell>, Vec<Position>)> {
	let chunked_positions = positions
		.chunks(dht_parallelization_limit)
		.map(|positions| {
			positions
				.iter()
				.map(|position| fetch_cell_from_dht(network_client, block_number, position))
				.collect::<Vec<_>>()
		})
		.collect::<Vec<_>>();

	let mut results = Vec::<Result<Cell>>::with_capacity(positions.len());
	for positions in chunked_positions {
		let r = join_all(positions).await;
		results.extend(r);
	}

	let (fetched, unfetched): (Vec<_>, Vec<_>) = results
		.into_iter()
		.zip(positions)
		.partition(|(res, _)| res.is_ok());

	for (result, position) in fetched.iter().chain(unfetched.iter()) {
		let reference = position.reference(block_number);
		match result {
			Ok(_) => debug!("Fetched cell {reference} from DHT"),
			Err(error) => debug!("Error fetching cell {reference} from DHT: {error}"),
		}
	}

	let fetched = fetched
		.into_iter()
		.map(|(result, _)| result)
		.collect::<Result<Vec<_>>>()?;

	let unfetched = unfetched
		.into_iter()
		.map(|(_, position)| position.clone())
		.collect::<Vec<_>>();

	Ok((fetched, unfetched))
}

fn store_data_in_db(db: Arc<DB>, app_id: u32, block_number: u32, data: &[u8]) -> Result<()> {
	let key = format!("{app_id}:{block_number}");
	let cf_handle = db
		.cf_handle(APP_DATA_CF)
		.context("Failed to get cf handle")?;

	db.put_cf(&cf_handle, key.as_bytes(), data)
		.context("Failed to write application data")
}

fn get_data_from_db(db: Arc<DB>, app_id: u32, block_number: u32) -> Result<Option<Vec<u8>>> {
	let key = format!("{app_id}:{block_number}");
	let cf_handle = db
		.cf_handle(crate::consts::APP_DATA_CF)
		.context("Couldn't get column handle from db")?;

	db.get_cf(&cf_handle, key.as_bytes())
		.context("Couldn't get app_data from db")
}

/// Encodes and stores app data into database under the `app_id:block_number` key
pub fn store_encoded_data_in_db<T: Encode>(
	db: Arc<DB>,
	app_id: u32,
	block_number: u32,
	data: &T,
) -> Result<()> {
	store_data_in_db(db, app_id, block_number, &data.encode())
}

/// Gets and decodes app data from database for the `app_id:block_number` key
pub fn get_decoded_data_from_db<T: Decode>(
	db: Arc<DB>,
	app_id: u32,
	block_number: u32,
) -> Result<Option<T>> {
	let res = get_data_from_db(db, app_id, block_number)
		.map(|e| e.map(|v| <T>::decode(&mut &v[..]).context("Failed decoding the app data.")));

	match res {
		Ok(Some(Err(e))) => Err(e),
		Ok(Some(Ok(s))) => Ok(Some(s)),
		Ok(None) => Ok(None),
		Err(e) => Err(e),
	}
}

/// Checks if block header for given block number is in database
pub fn is_block_header_in_db(db: Arc<DB>, block_number: u32) -> Result<bool> {
	let handle = db
		.cf_handle(BLOCK_HEADER_CF)
		.context("Failed to get cf handle")?;

	db.get_pinned_cf(&handle, block_number.to_be_bytes())
		.context("Failed to get block header")
		.map(|value| value.is_some())
}

/// Stores block header into database under the given block number key
pub fn store_block_header_in_db(db: Arc<DB>, block_number: u32, header: &DaHeader) -> Result<()> {
	let handle = db
		.cf_handle(BLOCK_HEADER_CF)
		.context("Failed to get cf handle")?;

	db.put_cf(
		&handle,
		block_number.to_be_bytes(),
		serde_json::to_string(header)?.as_bytes(),
	)
	.context("Failed to write block header")
}

/// Checks if confidence factor for given block number is in database
pub fn is_confidence_in_db(db: Arc<DB>, block_number: u32) -> Result<bool> {
	let handle = db
		.cf_handle(CONFIDENCE_FACTOR_CF)
		.context("Failed to get cf handle")?;

	db.get_pinned_cf(&handle, block_number.to_be_bytes())
		.context("Failed to get confidence")
		.map(|value| value.is_some())
}

/// Gets confidence factor from database for given block number
pub fn get_confidence_from_db(db: Arc<DB>, block_number: u32) -> Result<Option<u32>> {
	let cf_handle = db
		.cf_handle(crate::consts::CONFIDENCE_FACTOR_CF)
		.context("Couldn't get column handle from db")?;

	let res = db
		.get_cf(&cf_handle, &block_number.to_be_bytes())
		.context("Couldn't get confidence in db")?;
	let res = res.map(|data| {
		data.try_into()
			.map_err(|_| anyhow!("Conversion failed"))
			.context("Unable to convert confindence (wrong number of bytes)")
			.map(u32::from_be_bytes)
	});

	match res {
		Some(Ok(r)) => Ok(Some(r)),
		None => Ok(None),
		Some(Err(e)) => Err(e),
	}
}

/// Stores confidence factor into database under the given block number key
pub fn store_confidence_in_db(db: Arc<DB>, block_number: u32, count: u32) -> Result<()> {
	let handle = db
		.cf_handle(CONFIDENCE_FACTOR_CF)
		.context("Failed to get cf handle")?;

	db.put_cf(&handle, block_number.to_be_bytes(), count.to_be_bytes())
		.context("Failed to write confidence")
}
