use std::{collections::HashMap, fs, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use thiserror::Error;
use tokio::sync::{oneshot, Mutex};

use libp2p::{
	core::{
		either::EitherTransport,
		muxing::StreamMuxerBox,
		transport,
		upgrade::{SelectUpgrade, Version},
	},
	identity::{
		ed25519::{Keypair as ed25519Key, SecretKey},
		Keypair,
	},
	kad::{
		record::Key,
		store::{MemoryStore, MemoryStoreConfig},
		Kademlia, KademliaConfig, PeerRecord, QueryId, Quorum, Record,
	},
	mplex::MplexConfig,
	noise::{self, NoiseConfig, X25519Spec},
	pnet::{PnetConfig, PreSharedKey},
	swarm::SwarmBuilder,
	tcp::{GenTcpConfig, TokioTcpTransport},
	yamux::YamuxConfig,
	Multiaddr, PeerId, Swarm, Transport,
};
use tracing::log::info;

#[derive(Debug, Error)]
#[error("Trying to use kad before bootstrap completed successfully.")]
pub struct NotBootstrapped;

#[derive(Debug, Error)]
#[error("{0:?}")]
pub struct KadStoreError(pub libp2p::kad::record::store::Error);

type GetRecordChannel = oneshot::Receiver<Result<Vec<PeerRecord>>>;
type PutRecordChannel = oneshot::Receiver<Result<()>>;

enum QueryChannel {
	GetRecord(oneshot::Sender<Result<Vec<PeerRecord>>>),
	PutRecord(oneshot::Sender<Result<()>>),
}

struct P2P {
	swarm: Swarm<Kademlia<MemoryStore>>,
	kad_queries: HashMap<QueryId, QueryChannel>,
	bootstrap_complete: bool,
}

impl P2P {
	fn kad_query_get_record(&mut self, key: Key, quorum: Quorum) -> GetRecordChannel {
		let (tx, rx) = oneshot::channel();
		if self.bootstrap_complete {
			let kad = self.swarm.behaviour_mut();
			let id = kad.get_record(key, quorum);
			self.kad_queries
				.insert(id.into(), QueryChannel::GetRecord(tx));
		} else {
			tx.send(Err(NotBootstrapped.into())).ok();
		}
		rx
	}

	fn kad_query_put_record(&mut self, record: Record, quorum: Quorum) -> PutRecordChannel {
		let (tx, rx) = oneshot::channel();
		if self.bootstrap_complete {
			let kad = self.swarm.behaviour_mut();
			match kad.put_record(record, quorum) {
				Ok(id) => {
					self.kad_queries
						.insert(id.into(), QueryChannel::PutRecord(tx));
				},
				Err(err) => {
					tx.send(Err(KadStoreError(err).into())).ok();
				},
			}
		} else {
			tx.send(Err(NotBootstrapped.into())).ok();
		}
		rx
	}
}

struct NetworkService(Arc<Mutex<P2P>>);

impl NetworkService {
	pub fn init(
		seed: u64,
		port: u16,
		bootstrap_nodes: Vec<(PeerId, Multiaddr)>,
		psk: Option<PreSharedKey>,
	) -> Result<Self> {
		// create peer id
		let keypair = keypair(seed)?;
		let local_peer_id = PeerId::from(keypair.public());
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
					behaviour.add_address(&mut peer.0, peer.1);
				}
			}

			SwarmBuilder::new(transport, behaviour, local_peer_id)
				// connection background tasks are spawned onto the tokio runtime
				.executor(Box::new(|fut| {
					tokio::spawn(fut);
				}))
				.build()
		};
		// listen on all interfaces and whatever port the OS assigns
		swarm.listen_on(format!("/ip4/0.0.0.0/tcp/{}", port).parse()?)?;

		Ok(NetworkService(Arc::new(Mutex::new(P2P {
			swarm,
			kad_queries: HashMap::default(),
			bootstrap_complete: false,
		}))))
	}

	pub async fn get_record(&self, key: Key, quorum: Quorum) -> Result<Vec<PeerRecord>> {
		let rx = {
			let mut p2p = self.0.lock().await;
			p2p.kad_query_get_record(key, quorum)
		};
		Ok(rx.await??)
	}

	pub async fn put_record(&self, record: Record, quorum: Quorum) -> Result<()> {
		let rx = {
			let mut p2p = self.0.lock().await;
			p2p.kad_query_put_record(record, quorum)
		};
		rx.await??;
		Ok(())
	}
}

fn get_psk(path: &Path) -> std::io::Result<Option<String>> {
	let swarm_key_file = path.join("swarm.key");
	match fs::read_to_string(swarm_key_file) {
		Ok(text) => Ok(Some(text)),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
		Err(e) => Err(e),
	}
}

fn setup_transport(
	key_pair: Keypair,
	psk: Option<PreSharedKey>,
) -> transport::Boxed<(PeerId, StreamMuxerBox)> {
	let dh_keys = noise::Keypair::<X25519Spec>::new()
		.into_authentic(&key_pair)
		.unwrap();
	let noise_cfg = NoiseConfig::xx(dh_keys).into_authenticated();

	let base_transport =
		TokioTcpTransport::new(GenTcpConfig::default().nodelay(true).port_reuse(false));
	let maybe_encrypted = match psk {
		Some(psk) => EitherTransport::Left(
			base_transport.and_then(move |socket, _| PnetConfig::new(psk).handshake(socket)),
		),
		None => EitherTransport::Right(base_transport),
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

fn keypair(i: u64) -> Result<Keypair> {
	let mut keypair = [0; 32];
	keypair[..8].copy_from_slice(&i.to_be_bytes());
	let secret = SecretKey::from_bytes(keypair).context("Cannot create keypair")?;
	Ok(Keypair::Ed25519(ed25519Key::from(secret)))
}
