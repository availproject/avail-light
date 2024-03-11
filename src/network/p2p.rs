use allow_block_list::BlockedPeers;
use color_eyre::{eyre::WrapErr, Report, Result};
use libp2p::{
	autonat, dcutr, identify, identity,
	kad::{self, PeerRecord, QueryId},
	mdns, noise, ping, relay,
	swarm::NetworkBehaviour,
	tcp, upnp, yamux, PeerId, Swarm, SwarmBuilder,
};
use multihash::{self, Hasher};
use std::collections::HashMap;
use tokio::sync::{
	mpsc::{self},
	oneshot,
};
use tracing::info;

#[cfg(feature = "network-analysis")]
pub mod analyzer;
mod client;
mod event_loop;
mod kad_mem_store;

use crate::types::{LibP2PConfig, SecretKey};
pub use client::Client;
pub use event_loop::EventLoop;
pub use kad_mem_store::MemoryStoreConfig;

use self::{client::BlockStat, kad_mem_store::MemoryStore};
use libp2p_allow_block_list as allow_block_list;

#[derive(Debug)]
pub enum QueryChannel {
	GetRecord(oneshot::Sender<Result<PeerRecord>>),
	PutRecord,
	Bootstrap(oneshot::Sender<Result<()>>),
}

pub struct EventLoopEntries<'a> {
	swarm: &'a mut Swarm<Behaviour>,
	pending_kad_queries: &'a mut HashMap<QueryId, QueryChannel>,
	pending_swarm_events: &'a mut HashMap<PeerId, oneshot::Sender<Result<()>>>,
	/// <block_num, (total_cells, result_cell_counter, time_stat)>
	active_blocks: &'a mut HashMap<u32, BlockStat>,
}

impl<'a> EventLoopEntries<'a> {
	pub fn new(
		swarm: &'a mut Swarm<Behaviour>,
		pending_kad_queries: &'a mut HashMap<QueryId, QueryChannel>,
		pending_swarm_events: &'a mut HashMap<PeerId, oneshot::Sender<Result<()>>>,
		active_blocks: &'a mut HashMap<u32, BlockStat>,
	) -> Self {
		Self {
			swarm,
			pending_kad_queries,
			pending_swarm_events,
			active_blocks,
		}
	}

	pub fn insert_query(&mut self, query_id: QueryId, result_sender: QueryChannel) {
		self.pending_kad_queries.insert(query_id, result_sender);
	}

	pub fn insert_swarm_event(
		&mut self,
		peer_id: PeerId,
		result_sender: oneshot::Sender<Result<()>>,
	) {
		self.pending_swarm_events.insert(peer_id, result_sender);
	}

	pub fn behavior_mut(&mut self) -> &mut Behaviour {
		self.swarm.behaviour_mut()
	}

	pub fn swarm(&mut self) -> &mut Swarm<Behaviour> {
		self.swarm
	}
}

pub trait Command {
	fn run(&mut self, entries: EventLoopEntries) -> Result<(), Report>;
	fn abort(&mut self, error: Report);
}

type SendableCommand = Box<dyn Command + Send + Sync>;
type CommandSender = mpsc::UnboundedSender<SendableCommand>;
type CommandReceiver = mpsc::UnboundedReceiver<SendableCommand>;

// Behaviour struct is used to derive delegated Libp2p behaviour implementation
#[derive(NetworkBehaviour)]
#[behaviour(event_process = false)]
pub struct Behaviour {
	kademlia: kad::Behaviour<MemoryStore>,
	identify: identify::Behaviour,
	ping: ping::Behaviour,
	mdns: mdns::tokio::Behaviour,
	auto_nat: autonat::Behaviour,
	relay_client: relay::client::Behaviour,
	dcutr: dcutr::Behaviour,
	upnp: upnp::tokio::Behaviour,
	blocked_peers: allow_block_list::Behaviour<BlockedPeers>,
}

fn generate_config(config: libp2p::swarm::Config, cfg: &LibP2PConfig) -> libp2p::swarm::Config {
	config
		.with_idle_connection_timeout(cfg.connection_idle_timeout)
		.with_max_negotiating_inbound_streams(cfg.max_negotiating_inbound_streams)
		.with_notify_handler_buffer_size(cfg.task_command_buffer_size)
		.with_dial_concurrency_factor(cfg.dial_concurrency_factor)
		.with_per_connection_event_buffer_size(cfg.per_connection_event_buffer_size)
}

async fn build_swarm(
	cfg: &LibP2PConfig,
	id_keys: &libp2p::identity::Keypair,
	kad_store: MemoryStore,
	is_ws_transport: bool,
) -> Result<Swarm<Behaviour>> {
	// create Identify Protocol Config
	let identify_cfg =
		identify::Config::new(cfg.identify.protocol_version.clone(), id_keys.public())
			.with_agent_version(cfg.identify.agent_version.to_string());

	// create AutoNAT Client Config
	let autonat_cfg = autonat::Config {
		retry_interval: cfg.autonat.retry_interval,
		refresh_interval: cfg.autonat.refresh_interval,
		boot_delay: cfg.autonat.boot_delay,
		throttle_server_period: cfg.autonat.throttle_server_period,
		only_global_ips: cfg.autonat.only_global_ips,
		..Default::default()
	};

	// build the Swarm, connecting the lower transport logic with the
	// higher layer network behaviour logic
	let tokio_swarm = SwarmBuilder::with_existing_identity(id_keys.clone()).with_tokio();

	let mut swarm;

	let behaviour = |key: &identity::Keypair, relay_client| {
		Ok(Behaviour {
			ping: ping::Behaviour::new(ping::Config::new()),
			identify: identify::Behaviour::new(identify_cfg),
			relay_client,
			dcutr: dcutr::Behaviour::new(key.public().to_peer_id()),
			kademlia: kad::Behaviour::with_config(key.public().to_peer_id(), kad_store, cfg.into()),
			auto_nat: autonat::Behaviour::new(key.public().to_peer_id(), autonat_cfg),
			mdns: mdns::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?,
			upnp: upnp::tokio::Behaviour::default(),
			blocked_peers: allow_block_list::Behaviour::default(),
		})
	};

	if is_ws_transport {
		swarm = tokio_swarm
			.with_websocket(noise::Config::new, yamux::Config::default)
			.await?
			.with_relay_client(noise::Config::new, yamux::Config::default)?
			.with_behaviour(behaviour)?
			.with_swarm_config(|c| generate_config(c, cfg))
			.build();
	} else {
		swarm = tokio_swarm
			.with_tcp(
				tcp::Config::default().port_reuse(false).nodelay(false),
				noise::Config::new,
				yamux::Config::default,
			)?
			.with_dns()?
			.with_relay_client(noise::Config::new, yamux::Config::default)?
			.with_behaviour(behaviour)?
			.with_swarm_config(|c| generate_config(c, cfg))
			.build();
	}

	info!("Local peerID: {}", swarm.local_peer_id());

	// Setting the mode this way disables automatic mode changes.
	//
	// Because the identify protocol doesn't allow us to change
	// agent data on the fly, we're forced to use static Kad modes
	// instead of relying on dynamic changes
	swarm
		.behaviour_mut()
		.kademlia
		.set_mode(Some(cfg.kademlia.kademlia_mode.into()));

	Ok(swarm)
}

// Keypair function creates identity Keypair for a local node.
// From such generated keypair it derives multihash identifier of the local peer.
pub fn keypair(cfg: &LibP2PConfig) -> Result<(libp2p::identity::Keypair, String)> {
	let keypair = match cfg.secret_key.as_ref() {
		// If seed is provided, generate secret key from seed
		Some(SecretKey::Seed { seed }) => {
			let seed_digest = multihash::Sha3_256::digest(seed.as_bytes());
			identity::Keypair::ed25519_from_bytes(seed_digest)
				.wrap_err("error generating secret key from seed")?
		},
		// Import secret key if provided
		Some(SecretKey::Key { key }) => {
			let mut decoded_key = [0u8; 32];
			hex::decode_to_slice(key.clone().into_bytes(), &mut decoded_key)
				.wrap_err("error decoding secret key from config")?;
			identity::Keypair::ed25519_from_bytes(decoded_key)
				.wrap_err("error importing secret key")?
		},
		// If neither seed nor secret key provided, generate secret key from random seed
		None => identity::Keypair::generate_ed25519(),
	};
	let peer_id = PeerId::from(keypair.public()).to_string();
	Ok((keypair, peer_id))
}
