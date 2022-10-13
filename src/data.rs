//! Persistence to DHT and RocksDB.

use std::{
	fs,
	path::Path,
	str::FromStr,
	sync::Arc,
	time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use async_std::stream::StreamExt;
use codec::{Decode, Encode};
use futures::{future::join_all, stream};

use kate_recovery::com::{Cell, Position};
use libp2p::{
	core::{
		either::EitherTransport,
		muxing::StreamMuxerBox,
		transport,
		upgrade::{SelectUpgrade, Version},
	},
	identity,
	kad::{
		store::{MemoryStore, MemoryStoreConfig},
		Kademlia, KademliaConfig,
	},
	mplex::MplexConfig,
	noise::{self, NoiseConfig, NoiseConfig, X25519Spec},
	pnet::{PnetConfig, PreSharedKey},
	swarm::SwarmBuilder,
	tcp::{GenTcpConfig, TokioTcpTransport},
	yamux::YamuxConfig,
	Multiaddr, PeerId, Swarm, Transport,
};
use rocksdb::DB;
use tracing::{debug, info, trace};

use crate::{
	consts::{APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF},
	types::Header,
};

fn setup_transport(
	key_pair: identity::Keypair,
	psk: Option<PreSharedKey>,
) -> transport::Boxed<(PeerId, StreamMuxerBox)> {
	let dh_keys = noise::Keypair::<X25519Spec>::new()
		.into_authentic(&id_keys)
		.expect("Signing libp2p-noise static DH keypair failed.");
	let noise_cfg = NoiseConfig::xx(dh_keys).into_authenticated();

	let base_transport =
		TokioTcpTransport::new(GenTcpConfig::default().nodelay(true).port_reuse(false));
	let maybe_encrypted = match psk {
		Some(psk) => {
			base_transport.and_then(move |socket, _| PnetConfig::new(psk).handshake(socket))
		},
		None => base_transport,
	};

	maybe_encrypted
		.upgrade(Version::V1)
		.authenticate(noise_cfg)
		.multiplex(SelectUpgrade::new(
			YamuxConfig::default(),
			MplexConfig::new(),
		))
		.timeout(Duration::from_secs(20))
		.boxed()
}

/// Read the pre-shared key file from the given directory
pub fn get_psk(path: &Path) -> std::io::Result<Option<String>> {
	let swarm_key_file = path.join("swarm.key");
	match fs::read_to_string(swarm_key_file) {
		Ok(text) => Ok(Some(text)),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
		Err(e) => Err(e),
	}
}

pub fn init_swarm(
	seed: u64,
	port: u16,
	bootstrap_nodes: Vec<(PeerId, Multiaddr)>,
	psk: Option<PreSharedKey>,
) -> anyhow::Result<Swarm<Kademlia<MemoryStore>>> {
	// create peer id
	let keypair = keypair(seed)?;
	let local_public_key = identity::PublicKey::Ed25519(keypair.public());
	let local_peer_id = PeerId::from(local_public_key);
	info!("Local peer id: {:?}", local_peer_id);

	// create transport
	let transport = setup_transport(keypair, psk);

	// create swarm that manages peers and events
	let swarm = {
		let kad_cfg = KademliaConfig::default();
		kad_cfg.set_query_timeout(Duration::from_secs(5 * 60));
		let store_cfg = MemoryStoreConfig {
			max_records: 24000000, // ~2hrs
			max_value_bytes: 100,
			max_providers_per_key: 1,
			max_provided_keys: 100000,
		};
		let kad_store = MemoryStore::with_config(local_peer_id, store_cfg);
		let behaviour = Kademlia::with_config(local_peer_id, kad_store, kad_cfg);

		// add configured boot nodes
		if !bootstrap_nodes.is_empty() {
			for peer in bootstrap_nodes {
				behaviour.add_address(&mut peer.0, peer.1)
			}
		}

		SwarmBuilder::new(transport, behaviour, local_peer_id)
			// connection background tasks are spawned onto the tokio runtime
			.executor(Box::new(|fut| tokio::spawn(fut)))
			.build()
	};
	// listen on all interfaces and whatever port the OS assigns
	swarm.listen_on(format!("/ip4/0.0.0.0/tcp/{}", port).parse()?)?;

	Ok(swarm)
}

fn keypair(i: u64) -> Result<Keypair> {
	let mut keypair = [0; 32];
	keypair[..8].copy_from_slice(&i.to_be_bytes());
	let secret = SecretKey::from_bytes(keypair).context("Cannot create keypair")?;
	Ok(Keypair::from(secret))
}

async fn fetch_cell_from_dht(
	swarm: &Swarm<Kademlia<MemoryStore>>,
	block_number: u64,
	position: &Position,
) -> Result<Cell> {
	let reference = position.reference(block_number);
	let record_key = Key::from(reference.as_bytes().to_vec());

	trace!("Getting DHT record for reference {}", reference);
	// For now, we take only the first record from the list
	swarm
		.behaviour_mut()
		.get_record(record_key, Quorum::One)
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
	fn reference(&self, block: u64) -> String {
		self.0.reference(block)
	}

	fn dht_record(&self, block: u64, ttl: u64) -> Record {
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
/// * `swarm` - Reference to Libp2p swarm component
/// * `block` - Block number
/// * `cells` - Matrix cells to store into DHT
/// * `dht_parallelization_limit` - Number of cells to fetch in parallel
/// * `ttl` - Cell time to live in DHT (in seconds)
pub async fn insert_into_dht(
	swarm: &Swarm<Kademlia<MemoryStore>>,
	block: u64,
	cells: Vec<Cell>,
	dht_parallelization_limit: usize,
	ttl: u64,
) {
	let cells: Vec<_> = cells.into_iter().map(DHTCell).collect::<Vec<_>>();
	let cell_tuples = cells.iter().map(move |b| (b, swarm.clone()));
	futures::StreamExt::for_each_concurrent(
		stream::iter(cell_tuples),
		dht_parallelization_limit,
		|(cell, swarm)| async move {
			let reference = cell.reference(block);
			if let Err(error) = swarm
				.behaviour_mut()
				.put_record(cell.dht_record(block, ttl), Quorum::One)
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
/// * `swarm` - Reference to Libp2p swarm component
/// * `block_number` - Block number
/// * `positions` - Cell positions to fetch
/// * `dht_parallelization_limit` - Number of cells to fetch in parallel
pub async fn fetch_cells_from_dht(
	swarm: &Swarm<Kademlia<MemoryStore>>,
	block_number: u64,
	positions: &Vec<Position>,
	dht_parallelization_limit: usize,
) -> Result<(Vec<Cell>, Vec<Position>)> {
	let chunked_positions = positions
		.chunks(dht_parallelization_limit)
		.map(|positions| {
			positions
				.iter()
				.map(|position| fetch_cell_from_dht(swarm, block_number, position))
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

fn store_data_in_db(db: Arc<DB>, app_id: u32, block_number: u64, data: &[u8]) -> Result<()> {
	let key = format!("{app_id}:{block_number}");
	let cf_handle = db
		.cf_handle(APP_DATA_CF)
		.context("Failed to get cf handle")?;

	db.put_cf(&cf_handle, key.as_bytes(), data)
		.context("Failed to write application data")
}

fn get_data_from_db(db: Arc<DB>, app_id: u32, block_number: u64) -> Result<Option<Vec<u8>>> {
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
	block_number: u64,
	data: &T,
) -> Result<()> {
	store_data_in_db(db, app_id, block_number, &data.encode())
}

/// Gets and decodes app data from database for the `app_id:block_number` key
pub fn get_decoded_data_from_db<T: Decode>(
	db: Arc<DB>,
	app_id: u32,
	block_number: u64,
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
pub fn is_block_header_in_db(db: Arc<DB>, block_number: u64) -> Result<bool> {
	let handle = db
		.cf_handle(BLOCK_HEADER_CF)
		.context("Failed to get cf handle")?;

	db.get_pinned_cf(&handle, block_number.to_be_bytes())
		.context("Failed to get block header")
		.map(|value| value.is_some())
}

/// Stores block header into database under the given block number key
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

/// Checks if confidence factor for given block number is in database
pub fn is_confidence_in_db(db: Arc<DB>, block_number: u64) -> Result<bool> {
	let handle = db
		.cf_handle(CONFIDENCE_FACTOR_CF)
		.context("Failed to get cf handle")?;

	db.get_pinned_cf(&handle, block_number.to_be_bytes())
		.context("Failed to get confidence")
		.map(|value| value.is_some())
}

/// Gets confidence factor from database for given block number
pub fn get_confidence_from_db(db: Arc<DB>, block_number: u64) -> Result<Option<u32>> {
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
pub fn store_confidence_in_db(db: Arc<DB>, block_number: u64, count: u32) -> Result<()> {
	let handle = db
		.cf_handle(CONFIDENCE_FACTOR_CF)
		.context("Failed to get cf handle")?;

	db.put_cf(&handle, block_number.to_be_bytes(), count.to_be_bytes())
		.context("Failed to write confidence")
}
