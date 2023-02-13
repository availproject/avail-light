mod client;
mod event_loop;

use std::time::Duration;

use libp2p::{autonat, core::ConnectedPoint, dns::TokioDnsConfig};

pub use client::Client;
use event_loop::{EventLoop, NetworkBehaviour};

use anyhow::{Context, Ok, Result};
use libp2p::{
	core::{muxing::StreamMuxerBox, transport, upgrade::Version},
	identify,
	identity::{self, ed25519, Keypair},
	kad::{
		store::{MemoryStore, MemoryStoreConfig},
		KademliaCaching, KademliaConfig,
	},
	metrics::Metrics,
	noise::NoiseAuthenticated,
	swarm::SwarmBuilder,
	tcp::{GenTcpConfig, TokioTcpTransport},
	yamux::YamuxConfig,
	PeerId, Transport,
};
use tokio::sync::mpsc;
use tracing::info;

use crate::types::{LibP2PConfig, SecretKey};

use multihash::{self, Hasher};

// Event enum encodes all used network event variants
#[derive(Debug, Clone)]
pub enum Event {
	ConnectionEstablished {
		peer_id: PeerId,
		endpoint: ConnectedPoint,
	},
}

pub fn init(
	cfg: LibP2PConfig,
	metrics: Metrics,
	dht_parallelization_limit: usize,
	ttl: u64,
) -> Result<(Client, EventLoop)> {
	// Create a public/private key pair, either based on a seed or random
	let id_keys = match cfg.libp2p_secret_key {
		// If seed is provided, generate secret key from seed
		Some(SecretKey::Seed { seed }) => {
			let seed_digest = multihash::Sha3_256::digest(seed.as_bytes());
			let secret_key = ed25519::SecretKey::from_bytes(seed_digest)
				.context("error generating secret key from seed")?;
			identity::Keypair::Ed25519(secret_key.into())
		},
		// Import secret key if provided
		Some(SecretKey::Key { key }) => {
			let mut decoded_key = [0u8; 32];
			hex::decode_to_slice(key.into_bytes(), &mut decoded_key)
				.context("error decoding secret key from config")?;
			let secret_key = ed25519::SecretKey::from_bytes(decoded_key)
				.context("error importing secret key")?;
			identity::Keypair::Ed25519(secret_key.into())
		},
		// If neither seed nor secret key provided, generate secret key from random seed
		None => identity::Keypair::generate_ed25519(),
	};
	let local_peer_id = PeerId::from(id_keys.public());
	info!(
		"Local peer id: {:?}. Public key: {:?}.",
		local_peer_id,
		id_keys.public()
	);

	// create transport
	let transport = setup_transport(&id_keys, cfg.libp2p_tcp_port_reuse)?;

	// create swarm that manages peers and events
	let swarm = {
		let mut kad_cfg = KademliaConfig::default();
		kad_cfg
			.set_publication_interval(cfg.kademlia.publication_interval)
			.set_replication_interval(cfg.kademlia.record_replication_interval)
			.set_replication_factor(cfg.kademlia.record_replication_factor)
			.set_connection_idle_timeout(cfg.kademlia.connection_idle_timeout)
			.set_query_timeout(cfg.kademlia.query_timeout)
			.set_parallelism(cfg.kademlia.query_parallelism)
			.set_caching(KademliaCaching::Enabled {
				max_peers: cfg.kademlia.caching_max_peers,
			})
			.disjoint_query_paths(cfg.kademlia.disjoint_query_paths);

		let store_cfg = MemoryStoreConfig {
			max_records: cfg.kademlia.max_kad_record_number, // ~2hrs
			max_value_bytes: cfg.kademlia.max_kad_record_size + 1, // Add 1 to match kademlia size check (>=)
			max_providers_per_key: usize::from(cfg.kademlia.record_replication_factor), // Needs to match the replication factor, per libp2p docs
			max_provided_keys: cfg.kademlia.max_kad_provided_keys,
		};
		let kad_store = MemoryStore::with_config(local_peer_id, store_cfg);
		let identify_cfg =
			identify::Config::new("/avail_kad/id/1.0.0".to_string(), id_keys.public());
		let autonat_cfg = autonat::Config {
			only_global_ips: cfg.libp2p_autonat_only_global_ips,
			..Default::default()
		};
		let behaviour =
			NetworkBehaviour::new(local_peer_id, kad_store, kad_cfg, identify_cfg, autonat_cfg)?;

		// Build the Swarm, connecting the lower transport logic with the
		// higher layer network behaviour logic
		SwarmBuilder::new(transport, behaviour, local_peer_id)
			// connection background tasks are spawned onto the tokio runtime
			.executor(Box::new(|fut| {
				tokio::spawn(fut);
			}))
			.build()
	};

	let (command_sender, command_receiver) = mpsc::channel(10000);

	Ok((
		Client::new(command_sender, dht_parallelization_limit, ttl),
		EventLoop::new(swarm, command_receiver, metrics),
	))
}

fn setup_transport(
	key_pair: &Keypair,
	port_reuse: bool,
) -> Result<transport::Boxed<(PeerId, StreamMuxerBox)>> {
	let noise = NoiseAuthenticated::xx(&key_pair).unwrap();
	let dns_tcp = TokioDnsConfig::system(TokioTcpTransport::new(
		GenTcpConfig::new().nodelay(true).port_reuse(port_reuse),
	))?;

	Ok(dns_tcp
		.upgrade(Version::V1)
		.authenticate(noise)
		.multiplex(YamuxConfig::default())
		.timeout(Duration::from_secs(20))
		.boxed())
}
