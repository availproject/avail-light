use std::{
	str::FromStr,
	sync::Arc,
	time::{Duration, Instant},
};

use anyhow::{Context, Result};
use async_std::stream::StreamExt;
use futures::future::join_all;
use ipfs_embed::{
	identity::ed25519::{Keypair, SecretKey},
	DefaultParams, DefaultParams as IPFSDefaultParams, Ipfs, Key, Multiaddr, NetworkConfig, PeerId,
	Quorum, Record, StorageConfig,
};
use kate_recovery::com::{Cell, Position};
use rocksdb::DB;

use crate::{
	consts::{APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF},
	types::{Event, Header},
};

pub async fn init_ipfs(
	seed: u64,
	port: u16,
	path: &str,
	bootstrap_nodes: &[(PeerId, Multiaddr)],
) -> anyhow::Result<Ipfs<IPFSDefaultParams>> {
	let sweep_interval = Duration::from_secs(600);
	let path_buf = std::path::PathBuf::from_str(path)?;
	let storage = StorageConfig::new(Some(path_buf), None, 10, sweep_interval);
	let mut network = NetworkConfig::new(keypair(seed)?);
	network.mdns = None;
	network.kad = Some(ipfs_embed::config::KadConfig {
		max_records: 24000000, // ~2hrs
		max_value_bytes: 100,
		max_providers_per_key: 1,
		max_provided_keys: 100000,
	});

	let ipfs = Ipfs::<IPFSDefaultParams>::new(ipfs_embed::Config { storage, network }).await?;

	_ = ipfs.listen_on(format!("/ip4/127.0.0.1/tcp/{}", port).parse()?)?;

	if !bootstrap_nodes.is_empty() {
		ipfs.bootstrap(bootstrap_nodes).await?;
	} else {
		// If client is the first one on the network, wait for the second client ConnectionEstablished event to use it as bootstrap
		// DHT requires boostrap to complete in order to be able to insert new records
		let node = ipfs
			.swarm_events()
			.find_map(|event| {
				if let ipfs_embed::Event::ConnectionEstablished(peer_id, connected_point) = event {
					Some((peer_id, connected_point.get_remote_address().clone()))
				} else {
					None
				}
			})
			.await
			.context("Connection is not established")?;
		ipfs.bootstrap(&[node]).await?;
	}

	Ok(ipfs)
}

pub async fn log_ipfs_events(ipfs: Ipfs<IPFSDefaultParams>) {
	let mut events = ipfs.swarm_events();
	while let Some(event) = events.next().await {
		let event = match event {
			ipfs_embed::Event::NewListener(_) => Event::NewListener,
			ipfs_embed::Event::NewListenAddr(_, addr) => Event::NewListenAddr(addr),
			ipfs_embed::Event::ExpiredListenAddr(_, addr) => Event::ExpiredListenAddr(addr),
			ipfs_embed::Event::ListenerClosed(_) => Event::ListenerClosed,
			ipfs_embed::Event::NewExternalAddr(addr) => Event::NewExternalAddr(addr),
			ipfs_embed::Event::ExpiredExternalAddr(addr) => Event::ExpiredExternalAddr(addr),
			ipfs_embed::Event::Discovered(peer_id) => Event::Discovered(peer_id),
			ipfs_embed::Event::Unreachable(peer_id) => Event::Unreachable(peer_id),
			ipfs_embed::Event::Connected(peer_id) => Event::Connected(peer_id),
			ipfs_embed::Event::Disconnected(peer_id) => Event::Disconnected(peer_id),
			ipfs_embed::Event::Subscribed(peer_id, topic) => Event::Subscribed(peer_id, topic),
			ipfs_embed::Event::Unsubscribed(peer_id, topic) => Event::Unsubscribed(peer_id, topic),
			ipfs_embed::Event::Bootstrapped => Event::Bootstrapped,
			ipfs_embed::Event::NewInfo(peer_id) => Event::NewInfo(peer_id),
			_ => Event::Other, // TODO: Is there a purpose to handle those events?
		};
		log::trace!("Received event: {}", event);
	}
}

fn keypair(i: u64) -> Result<Keypair> {
	let mut keypair = [0; 32];
	keypair[..8].copy_from_slice(&i.to_be_bytes());
	let secret = SecretKey::from_bytes(keypair).context("Cannot create keypair")?;
	Ok(Keypair::from(secret))
}

async fn fetch_cell_from_dht(
	ipfs: &Ipfs<DefaultParams>,
	block_number: u64,
	position: &Position,
) -> Result<Cell> {
	let reference = position.reference(block_number);
	let record_key = Key::from(reference.as_bytes().to_vec());

	log::trace!("Getting DHT record for reference {}", reference);
	// For now, we take only the first record from the list
	ipfs.get_record(record_key, Quorum::One)
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

struct IpfsCell(Cell);

impl IpfsCell {
	fn reference(&self, block: u64) -> String { self.0.reference(block) }

	// TODO: Add TTL to all new records
	fn dht_record(&self, block: u64) -> Record {
		Record {
			key: self.0.reference(block).as_bytes().to_vec().into(),
			value: self.0.content.to_vec(),
			publisher: None,
			expires: Instant::now().checked_add(Duration::from_secs(3600)),
		}
	}
}

/// Inserts cells into the DHT.
/// There is no rollback, and errors will be logged and skipped,
/// which means that we cannot rely on error logs as alert mechanism.
///
/// # Arguments
///
/// * `ipfs` - Reference to IPFS node
/// * `block` - Block number
/// * `cells` - Matrix cells to store into IPFS
pub async fn insert_into_dht(ipfs: &Ipfs<DefaultParams>, block: u64, cells: Vec<Cell>) {
	for cell in cells.into_iter().map(IpfsCell) {
		let reference = cell.reference(block);
		if let Err(error) = ipfs.put_record(cell.dht_record(block), Quorum::One).await {
			log::info!("Fail to put record for cell {reference} to DHT: {error}");
		}
	}
}

pub async fn fetch_cells_from_dht(
	ipfs: &Ipfs<DefaultParams>,
	block_number: u64,
	positions: &Vec<Position>,
	max_parallel_fetch_tasks: usize,
) -> Result<(Vec<Cell>, Vec<Position>)> {
	let chunked_positions = positions
		.chunks(max_parallel_fetch_tasks)
		.map(|positions| {
			positions
				.iter()
				.map(|position| fetch_cell_from_dht(ipfs, block_number, position))
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
			Ok(_) => log::debug!("Fetched cell {reference} from DHT"),
			Err(error) => log::debug!("Error fetching cell {reference} from DHT: {error}"),
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

pub fn store_data_in_db(
	db: Arc<DB>,
	app_id: u32,
	block_number: u64,
	data: &Vec<Vec<u8>>,
) -> Result<()> {
	let key = format!("{app_id}:{block_number}");
	let cf_handle = db
		.cf_handle(APP_DATA_CF)
		.context("Failed to get cf handle")?;

	db.put_cf(
		&cf_handle,
		key.as_bytes(),
		serde_json::to_string(data)?.as_bytes(),
	)
	.context("Failed to write application data")
}

pub fn is_block_header_in_db(db: Arc<DB>, block_number: u64) -> Result<bool> {
	let handle = db
		.cf_handle(BLOCK_HEADER_CF)
		.context("Failed to get cf handle")?;

	db.get_pinned_cf(&handle, block_number.to_be_bytes())
		.context("Failed to get block header")
		.map(|value| value.is_some())
}

pub fn store_block_header_in_db(db: Arc<DB>, block_number: u64, header: &Header) -> Result<()> {
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

pub fn is_confidence_in_db(db: Arc<DB>, block_number: u64) -> Result<bool> {
	let handle = db
		.cf_handle(CONFIDENCE_FACTOR_CF)
		.context("Failed to get cf handle")?;

	db.get_pinned_cf(&handle, block_number.to_be_bytes())
		.context("Failed to get confidence")
		.map(|value| value.is_some())
}

pub fn store_confidence_in_db(db: Arc<DB>, block_number: u64, count: u32) -> Result<()> {
	let handle = db
		.cf_handle(CONFIDENCE_FACTOR_CF)
		.context("Failed to get cf handle")?;

	db.put_cf(&handle, block_number.to_be_bytes(), count.to_be_bytes())
		.context("Failed to write confidence")
}
