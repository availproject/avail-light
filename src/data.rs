extern crate anyhow;
extern crate ipfs_embed;
extern crate libipld;

use std::{str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use async_std::stream::StreamExt;
use futures::future::join_all;
use ipfs_embed::{
	identity::ed25519::{Keypair, SecretKey},
	Block, Cid, DefaultParams, DefaultParams as IPFSDefaultParams, Ipfs, Key, Multiaddr,
	NetworkConfig, PeerId, Record, StorageConfig,
};
use kate_recovery::com::{Cell, Position};
use libipld::{
	multihash::{Code, MultihashDigest},
	IpldCodec,
};
use multihash::Multihash;
use rocksdb::DB;

use crate::{consts::APP_DATA_CF, types::Event};

pub async fn init_ipfs(
	seed: u64,
	port: u16,
	path: &str,
	bootstrap_nodes: &[(PeerId, Multiaddr)],
) -> anyhow::Result<Ipfs<IPFSDefaultParams>> {
	let sweep_interval = Duration::from_secs(600);
	let path_buf = std::path::PathBuf::from_str(path).unwrap();
	let storage = StorageConfig::new(Some(path_buf), None, 10, sweep_interval);
	let mut network = NetworkConfig::new(keypair(seed));
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
			.expect("Connection established");
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

fn keypair(i: u64) -> Keypair {
	let mut keypair = [0; 32];
	keypair[..8].copy_from_slice(&i.to_be_bytes());
	let secret = SecretKey::from_bytes(keypair).unwrap();
	Keypair::from(secret)
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
		.get_record(record_key, ipfs_embed::Quorum::One)
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

fn cell_hash(cell: &Cell) -> Multihash { Code::Sha3_256.digest(&cell.content) }

fn cell_cid(cell: &Cell) -> Cid { Cid::new_v1(IpldCodec::Raw.into(), cell_hash(cell)) }

fn cell_to_ipfs_block(cell: Cell) -> Block<DefaultParams> {
	Block::<DefaultParams>::new(cell_cid(&cell), cell.content.to_vec()).unwrap()
}

fn cell_ipfs_record(cell: &Cell, block: u64) -> Record {
	Record::new(
		cell.reference(block).as_bytes().to_vec(),
		cell_cid(cell).to_bytes(),
	)
}

/// Inserts cells into IPFS along with corresponding DHT records. At the end, block store is flushed.
/// There is no rollback, and errors will be logged and skipped,
/// which means that we cannot rely on error logs as alert mechanism.
///
/// # Arguments
///
/// * `ipfs` - Reference to IPFS node
/// * `block` - Block number
/// * `cells` - Matrix cells to store into IPFS
pub async fn insert_into_ipfs(ipfs: &Ipfs<DefaultParams>, block: u64, cells: Vec<Cell>) {
	// TODO: Should we encode block data?
	// TODO: Should we optimize by storing multiple cells into single block (per AppID)?
	// TODO: Return error in case when there are not successful inserts or put?
	for cell in cells {
		let reference = cell.reference(block);
		let ipfs_block = cell_to_ipfs_block(cell.clone());
		if let Err(error) = ipfs.insert(ipfs_block) {
			log::info!("Fail to insert cell {reference} into IPFS: {error}");
		}
		let dht_record = cell_ipfs_record(&cell, block);
		if let Err(error) = ipfs.put_record(dht_record, ipfs_embed::Quorum::One).await {
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
) -> Result<(Vec<Cell>, Vec<Position>)> {
	// TODO: Should we fetch peers before loop or for each cell?
	let peers = &ipfs.peers();
	log::debug!("Number of known IPFS peers: {}", peers.len());

	if peers.is_empty() {
		log::info!("No known IPFS peers");
		return Ok((vec![], positions.to_vec()));
	}

	let res = join_all(
		positions
			.iter()
			.map(|position| fetch_cell_from_ipfs(ipfs, peers.clone(), block_number, position))
			.collect::<Vec<_>>(),
	)
	.await;

	let (fetched, unfetched): (Vec<_>, Vec<_>) = res
		.into_iter()
		.zip(positions)
		.partition(|(res, _)| res.is_ok());

	let fetched = fetched
		.into_iter()
		.map(|e| {
			let cell = e.0.unwrap();
			log::debug!("Fetched cell {} from IPFS", cell.reference(block_number));
			cell
		})
		.collect::<Vec<_>>();

	let unfetched = unfetched
		.into_iter()
		.map(|(result, position)| {
			log::debug!(
				"Error fetching cell {} from IPFS: {}",
				position.reference(block_number),
				result.unwrap_err()
			);
			position.clone()
		})
		.collect::<Vec<_>>();

	log::info!("Number of cells fetched from IPFS: {}", fetched.len());
	Ok((fetched, unfetched))
}

pub fn store_data_in_db(
	db: Arc<DB>,
	app_id: u32,
	block_number: u64,
	data: Vec<Vec<u8>>,
) -> Result<()> {
	let key = format!("{app_id}:{block_number}");
	let cf_handle = db
		.cf_handle(APP_DATA_CF)
		.context("failed to get cf handle")?;

	db.put_cf(
		&cf_handle,
		key.as_bytes(),
		serde_json::to_string(&data)?.as_bytes(),
	)
	.context("failed to write application data")
}
