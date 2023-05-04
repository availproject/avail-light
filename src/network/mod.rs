mod client;
mod event_loop;

use crate::types::{LibP2PConfig, SecretKey};
use anyhow::{Context, Result};
pub use client::Client;
use event_loop::EventLoop;
use futures::future::Either;
use libp2p::{
	autonat::{self, Behaviour as AutoNat},
	core::{muxing::StreamMuxerBox, transport::OrTransport, upgrade::Version, ConnectedPoint},
	dcutr::Behaviour as Dcutr,
	dns::TokioDnsConfig,
	identify::{self, Behaviour as Identify},
	identity,
	kad::{
		store::{MemoryStore, MemoryStoreConfig},
		Kademlia, KademliaCaching, KademliaConfig,
	},
	mdns::{tokio::Behaviour as Mdns, Config as MdnsConfig},
	metrics::Metrics,
	noise::{Keypair, NoiseConfig, X25519Spec},
	ping::{Behaviour as Ping, Config as PingConfig},
	quic::{tokio::Transport as TokioQuic, Config as QuicConfig},
	relay::{self, client::Behaviour as RelayClient, Behaviour as Relay, Config as RelayConfig},
	swarm::{behaviour::toggle::Toggle, NetworkBehaviour, SwarmBuilder},
	tcp::{tokio::Transport as TokioTcpTransport, Config},
	yamux::{WindowUpdateMode, YamuxConfig},
	PeerId, Transport,
};
use multihash::{self, Hasher};
use tokio::sync::mpsc;
use tracing::info;

// Event enum encodes all used network event variants
#[derive(Debug, Clone)]
pub enum Event {
	ConnectionEstablished {
		peer_id: PeerId,
		endpoint: ConnectedPoint,
	},
}

#[derive(NetworkBehaviour)]
#[behaviour(event_process = false)]
pub struct Behaviour {
	relay: Toggle<Relay>,
	identify: Identify,
	relay_client: Toggle<RelayClient>,
	dcutr: Toggle<Dcutr>,
	kademlia: Kademlia<MemoryStore>,
	auto_nat: AutoNat,
	mdns: Toggle<Mdns>,
	ping: Ping,
}

pub fn init(
	cfg: LibP2PConfig,
	metrics: Metrics,
	dht_parallelization_limit: usize,
	ttl: u64,
	put_batch_size: usize,
) -> Result<(Client, EventLoop)> {
	// Create a public/private key pair, either based on a seed or random
	let id_keys = match cfg.libp2p_secret_key {
		// If seed is provided, generate secret key from seed
		Some(SecretKey::Seed { seed }) => {
			let seed_digest = multihash::Sha3_256::digest(seed.as_bytes());
			identity::Keypair::ed25519_from_bytes(seed_digest)
				.context("error generating secret key from seed")?
		},
		// Import secret key if provided
		Some(SecretKey::Key { key }) => {
			let mut decoded_key = [0u8; 32];
			hex::decode_to_slice(key.into_bytes(), &mut decoded_key)
				.context("error decoding secret key from config")?;
			identity::Keypair::ed25519_from_bytes(decoded_key)
				.context("error importing secret key")?
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

	let noise_keys = Keypair::<X25519Spec>::new().into_authentic(&id_keys)?;

	// Create Transport
	// relay transport configuration used in relay clients
	let local_peer_id = PeerId::from(id_keys.public());
	let (relay_client_transport, relay_client_behaviour) = relay::client::new(local_peer_id);

	let mut yamux_config = YamuxConfig::default();
	// enabling propper flow-control:
	// window updates are only sent when buffered data has been consumed
	yamux_config.set_window_update_mode(WindowUpdateMode::on_read());

	let tcp_transport = TokioTcpTransport::new(
		Config::new()
			.nodelay(true)
			.port_reuse(cfg.libp2p_tcp_port_reuse),
	);

	let quic_transport = {
		let mut quic_config = QuicConfig::new(&id_keys);
		quic_config.support_draft_29 = true;
		TokioQuic::new(QuicConfig::new(&id_keys))
	};

	// based on the need for a node to act as relaying server or not
	// it's underlaying TCP transport will change
	let transport = if cfg.is_relay {
		// if we're a relay
		// then we need to support TCP transport
		let relay_tcp_transport = tcp_transport
			.upgrade(Version::V1)
			.authenticate(NoiseConfig::xx(noise_keys).into_authenticated())
			.multiplex(yamux_config);

		TokioDnsConfig::system(OrTransport::new(quic_transport, relay_tcp_transport).map(
			|either_output, _| match either_output {
				Either::Left((peer_id, connection)) => (peer_id, StreamMuxerBox::new(connection)),
				Either::Right((peer_id, connection)) => (peer_id, StreamMuxerBox::new(connection)),
			},
		))?
		.boxed()
	} else {
		// but, if we're a client that will use Relaying
		// we need to mix relay client transport with TCP
		let relay_client_tcp_transport = OrTransport::new(relay_client_transport, tcp_transport)
			.upgrade(Version::V1)
			.authenticate(NoiseConfig::xx(noise_keys).into_authenticated())
			.multiplex(yamux_config);

		TokioDnsConfig::system(
			OrTransport::new(quic_transport, relay_client_tcp_transport).map(|either_output, _| {
				match either_output {
					Either::Left((peer_id, connection)) => {
						(peer_id, StreamMuxerBox::new(connection))
					},
					Either::Right((peer_id, connection)) => {
						(peer_id, StreamMuxerBox::new(connection))
					},
				}
			}),
		)?
		.boxed()
	};

	// Initialize Network Behaviour Struct
	// configure Kademlia Memory Store
	let kad_store = MemoryStore::with_config(
		local_peer_id,
		MemoryStoreConfig {
			max_records: cfg.kademlia.max_kad_record_number, // ~2hrs
			max_value_bytes: cfg.kademlia.max_kad_record_size + 1,
			max_providers_per_key: usize::from(cfg.kademlia.record_replication_factor), // Needs to match the replication factor, per libp2p docs
			max_provided_keys: cfg.kademlia.max_kad_provided_keys,
		},
	);
	// create Kademlia Config
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
	// create Indetify Protocol Config
	let identify_cfg = identify::Config::new("/avail_kad/id/1.0.0".to_string(), id_keys.public())
		.with_agent_version(agent_version());
	// create AutoNAT Config
	let autonat_cfg = autonat::Config {
		only_global_ips: cfg.libp2p_autonat_only_global_ips,
		..Default::default()
	};
	// toggle Relaying related behaviours
	// based on the need for a node to act as relaying server or not
	let behaviour = if cfg.is_relay {
		Behaviour {
			relay: Some(Relay::new(local_peer_id, RelayConfig::default())).into(),
			ping: Ping::new(PingConfig::new()),
			identify: Identify::new(identify_cfg),
			relay_client: None.into(),
			dcutr: None.into(),
			kademlia: Kademlia::with_config(local_peer_id, kad_store, kad_cfg),
			auto_nat: AutoNat::new(local_peer_id, autonat_cfg),
			mdns: None.into(),
		}
	} else {
		Behaviour {
			relay: None.into(),
			ping: Ping::new(PingConfig::new()),
			identify: Identify::new(identify_cfg),
			relay_client: Some(relay_client_behaviour).into(),
			dcutr: Some(Dcutr::new(local_peer_id)).into(),
			kademlia: Kademlia::with_config(local_peer_id, kad_store, kad_cfg),
			auto_nat: AutoNat::new(local_peer_id, autonat_cfg),
			mdns: Some(Mdns::new(MdnsConfig::default(), local_peer_id)?).into(),
		}
	};

	// Build the Swarm, connecting the lower transport logic with the
	// higher layer network behaviour logic
	let swarm = SwarmBuilder::with_tokio_executor(transport, behaviour, local_peer_id).build();

	// create sender channel for Event Loop Commands
	let (command_sender, command_receiver) = mpsc::channel(10000);

	Ok((
		Client::new(
			command_sender,
			dht_parallelization_limit,
			ttl,
			put_batch_size,
		),
		EventLoop::new(swarm, command_receiver, metrics, cfg.relays),
	))
}

fn agent_version() -> String {
	format!(
		"avail-light-client/rust-client/{}",
		env!("CARGO_PKG_VERSION")
	)
}
