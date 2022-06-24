use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use async_std::stream::StreamExt;
use futures::future::join_all;
use ipfs_embed::{
	identity::ed25519::{Keypair, SecretKey},
	Block, Cid, DefaultParams, DefaultParams as IPFSDefaultParams, Ipfs, Key, Multiaddr,
	NetworkConfig, PeerId, Quorum, Record, StorageConfig,
};
use kate_recovery::com::{Cell, Position};
use libipld::{
	multihash::{Code, MultihashDigest},
	IpldCodec,
};
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

async fn fetch_cell_from_ipfs(
	ipfs: &Ipfs<DefaultParams>,
	peers: Vec<PeerId>,
	block_number: u64,
	position: &Position,
) -> Result<Cell> {
	let reference = position.reference(block_number);
	let record_key = Key::from(reference.as_bytes().to_vec());

	log::trace!("Getting DHT record for reference {}", reference);
	let cid = ipfs
		.get_record(record_key, Quorum::One)
		.await
		.map(|record| record[0].record.value.to_vec())
		.and_then(|value| Cid::try_from(value).context("Invalid CID value"))?;

	log::trace!("Fetching IPFS block for CID {}", cid);
	ipfs.fetch(&cid, peers).await.and_then(|block| {
		Ok(Cell {
			position: position.clone(),
			content: block.data().try_into()?,
		})
	})
}

struct IpfsCell(Cell);

impl IpfsCell {
	fn reference(&self, block: u64) -> String { self.0.reference(block) }

	fn cid(&self) -> Cid {
		let hash = Code::Sha3_256.digest(&self.0.content);
		Cid::new_v1(IpldCodec::Raw.into(), hash)
	}

	fn ipfs_block(&self) -> Block<DefaultParams> {
		Block::<DefaultParams>::new(self.cid(), self.0.content.to_vec())
			.expect("hash matches the data")
	}

	fn dht_record(&self, block: u64) -> Record {
		Record::new(
			self.0.reference(block).as_bytes().to_vec(),
			self.cid().to_bytes(),
		)
	}
}

/// Inserts cells into IPFS along with corresponding DHT records. At the end, block store is flushed.
/// There is no rollback, and errors will be logged and skipped,
/// which means that we cannot rely on error logs as alert mechanism.
/// NOTE: Block data is not encoded and storing block per cell could be optimal solution.
///
/// # Arguments
///
/// * `ipfs` - Reference to IPFS node
/// * `block` - Block number
/// * `cells` - Matrix cells to store into IPFS
pub async fn insert_into_ipfs(ipfs: &Ipfs<DefaultParams>, block: u64, cells: Vec<Cell>) {
	for cell in cells.into_iter().map(IpfsCell) {
		let reference = cell.reference(block);
		if let Err(error) = ipfs.insert(cell.ipfs_block()) {
			log::info!("Fail to insert cell {reference} into IPFS: {error}",);
		}
		if let Err(error) = ipfs.put_record(cell.dht_record(block), Quorum::One).await {
			log::info!("Fail to put record for cell {reference} to DHT: {error}");
		}
	}
	if let Err(error) = ipfs.flush().await {
		log::info!("Cannot flush IPFS data to disk: {error}");
	};
}

pub async fn fetch_cells_from_ipfs(
	ipfs: &Ipfs<DefaultParams>,
	block_number: u64,
	positions: &Vec<Position>,
	max_parallel_fetch_tasks: usize,
) -> Result<(Vec<Cell>, Vec<Position>)> {
	// TODO: Should we fetch peers before loop or for each cell?
	let peers = &ipfs.peers();
	log::debug!("Number of known IPFS peers: {}", peers.len());

	if peers.is_empty() {
		log::info!("No known IPFS peers");
		return Ok((vec![], positions.to_vec()));
	}

	let chunked_positions = positions
		.chunks(max_parallel_fetch_tasks)
		.map(|positions| {
			positions
				.iter()
				.map(|position| fetch_cell_from_ipfs(ipfs, peers.clone(), block_number, position))
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
			Ok(_) => log::debug!("Fetched cell {reference} from IPFS"),
			Err(error) => log::debug!("Error fetching cell {reference} from IPFS: {error}"),
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
