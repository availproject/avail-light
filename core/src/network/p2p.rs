use allow_block_list::BlockedPeers;
use color_eyre::{eyre::WrapErr, Report, Result};
use libp2p::{
	autonat, dcutr, identify, identity,
	kad::{self, Mode, PeerRecord, QueryId},
	mdns, noise, ping, relay,
	swarm::NetworkBehaviour,
	tcp, upnp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use multihash::{self, Hasher};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fmt, net::Ipv4Addr, str::FromStr, time::Duration};
use tokio::sync::{
	mpsc::{self},
	oneshot,
};
use tracing::info;
#[cfg(feature = "network-analysis")]
pub mod analyzer;
mod client;
mod event_loop;
mod kad_mem_providers;
#[cfg(not(feature = "kademlia-rocksdb"))]
mod kad_mem_store;
mod kad_rocksdb_store;
use self::{client::BlockStat, event_loop::ConnectionEstablishedInfo};
use crate::types::{duration_seconds_format, LibP2PConfig, SecretKey};
pub use client::Client;
pub use event_loop::EventLoop;
pub use kad_mem_providers::ProvidersConfig;
#[cfg(not(feature = "kademlia-rocksdb"))]
pub use kad_mem_store::MemoryStoreConfig;
pub use kad_rocksdb_store::ExpirationCompactionFilterFactory;
pub use kad_rocksdb_store::RocksDBStoreConfig;
use libp2p_allow_block_list as allow_block_list;

const MINIMUM_SUPPORTED_BOOTSTRAP_VERSION: &str = "0.1.1";
const MINIMUM_SUPPORTED_LIGHT_CLIENT_VERSION: &str = "1.9.2";
const IDENTITY_PROTOCOL: &str = "/avail/light/1.0.0";
const IDENTITY_AGENT_BASE: &str = "avail-light-client";
const IDENTITY_AGENT_ROLE: &str = "light-client";
const IDENTITY_AGENT_CLIENT_TYPE: &str = "rust-client";

#[derive(Clone)]
struct AgentVersion {
	pub base_version: String,
	pub role: String,
	pub client_type: String,
	pub release_version: String,
}

impl fmt::Display for AgentVersion {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let AgentVersion {
			base_version,
			role,
			client_type,
			release_version,
		} = self;
		write!(f, "{base_version}/{role}/{release_version}/{client_type}",)
	}
}

impl FromStr for AgentVersion {
	type Err = String;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		let parts: Vec<&str> = s.split('/').collect();
		if parts.len() != 4 {
			return Err("Failed to parse agent version".to_owned());
		}

		Ok(AgentVersion {
			base_version: parts[0].to_string(),
			role: parts[1].to_string(),
			release_version: parts[2].to_string(),
			client_type: parts[3].to_string(),
		})
	}
}

impl AgentVersion {
	fn new(version: &str) -> Self {
		Self {
			base_version: IDENTITY_AGENT_BASE.to_string(),
			role: IDENTITY_AGENT_ROLE.to_string(),
			release_version: version.to_string(),
			client_type: IDENTITY_AGENT_CLIENT_TYPE.to_string(),
		}
	}

	fn is_supported(&self) -> bool {
		let minimum_version = if self.role == "bootstrap" {
			MINIMUM_SUPPORTED_BOOTSTRAP_VERSION
		} else {
			MINIMUM_SUPPORTED_LIGHT_CLIENT_VERSION
		};

		Version::parse(&self.release_version)
			.and_then(|release_version| {
				Version::parse(minimum_version).map(|min_version| release_version >= min_version)
			})
			.unwrap_or(false)
	}
}

#[derive(Debug)]
pub enum QueryChannel {
	GetRecord(oneshot::Sender<Result<PeerRecord>>),
	PutRecord,
	Bootstrap(oneshot::Sender<Result<()>>),
}

pub struct EventLoopEntries<'a> {
	swarm: &'a mut Swarm<Behaviour>,
	pending_kad_queries: &'a mut HashMap<QueryId, QueryChannel>,
	pending_swarm_events:
		&'a mut HashMap<PeerId, oneshot::Sender<Result<ConnectionEstablishedInfo>>>,
	/// <block_num, (total_cells, result_cell_counter, time_stat)>
	active_blocks: &'a mut HashMap<u32, BlockStat>,
	kad_mode: &'a mut Mode,
}

impl<'a> EventLoopEntries<'a> {
	pub fn new(
		swarm: &'a mut Swarm<Behaviour>,
		pending_kad_queries: &'a mut HashMap<QueryId, QueryChannel>,
		pending_swarm_events: &'a mut HashMap<
			PeerId,
			oneshot::Sender<Result<ConnectionEstablishedInfo>>,
		>,
		active_blocks: &'a mut HashMap<u32, BlockStat>,
		kad_mode: &'a mut Mode,
	) -> Self {
		Self {
			swarm,
			pending_kad_queries,
			pending_swarm_events,
			active_blocks,
			kad_mode,
		}
	}

	pub fn peer_id(&self) -> &PeerId {
		self.swarm.local_peer_id()
	}

	pub fn listeners(&self) -> Vec<String> {
		self.swarm.listeners().map(ToString::to_string).collect()
	}

	pub fn external_address(&self) -> Vec<String> {
		self.swarm
			.external_addresses()
			.map(ToString::to_string)
			.collect()
	}

	pub fn insert_query(&mut self, query_id: QueryId, result_sender: QueryChannel) {
		self.pending_kad_queries.insert(query_id, result_sender);
	}

	pub fn insert_swarm_event(
		&mut self,
		peer_id: PeerId,
		result_sender: oneshot::Sender<Result<ConnectionEstablishedInfo>>,
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

#[cfg(not(feature = "kademlia-rocksdb"))]
type Store = kad_mem_store::MemoryStore;
#[cfg(feature = "kademlia-rocksdb")]
type Store = kad_rocksdb_store::RocksDBStore;

// Behaviour struct is used to derive delegated Libp2p behaviour implementation
#[derive(NetworkBehaviour)]
#[behaviour(event_process = false)]
pub struct Behaviour {
	kademlia: kad::Behaviour<Store>,
	identify: identify::Behaviour,
	ping: ping::Behaviour,
	mdns: mdns::tokio::Behaviour,
	auto_nat: autonat::Behaviour,
	relay_client: relay::client::Behaviour,
	dcutr: dcutr::Behaviour,
	upnp: upnp::tokio::Behaviour,
	blocked_peers: allow_block_list::Behaviour<BlockedPeers>,
}

#[derive(Debug)]
pub struct PeerInfo {
	pub peer_id: String,
	pub operation_mode: String,
	pub peer_multiaddr: Option<Vec<String>>,
	pub local_listeners: Vec<String>,
	pub external_listeners: Vec<String>,
	pub public_listeners: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MultiAddressInfo {
	multiaddresses: Vec<String>,
	peer_id: String,
}

fn generate_config(config: libp2p::swarm::Config, cfg: &LibP2PConfig) -> libp2p::swarm::Config {
	config
		.with_idle_connection_timeout(cfg.connection_idle_timeout)
		.with_max_negotiating_inbound_streams(cfg.max_negotiating_inbound_streams)
		.with_notify_handler_buffer_size(cfg.task_command_buffer_size)
		.with_dial_concurrency_factor(cfg.dial_concurrency_factor)
		.with_per_connection_event_buffer_size(cfg.per_connection_event_buffer_size)
}

/// Libp2p AutoNAT configuration (see [RuntimeConfig] for details)
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct AutoNATConfig {
	/// Interval in which the NAT status should be re-tried if it is currently unknown or max confidence was not reached yet. (default: 10 sec)
	#[serde(with = "duration_seconds_format")]
	pub autonat_retry_interval: Duration,
	/// Interval in which the NAT should be tested again if max confidence was reached in a status. (default: 30 sec)
	#[serde(with = "duration_seconds_format")]
	pub autonat_refresh_interval: Duration,
	/// AutoNat on init delay before starting the fist probe. (default: 5 sec)
	#[serde(with = "duration_seconds_format")]
	pub autonat_boot_delay: Duration,
	/// AutoNat throttle period for re-using a peer as server for a dial-request. (default: 1 sec)
	#[serde(with = "duration_seconds_format")]
	pub autonat_throttle: Duration,
	/// Configures AutoNAT behaviour to reject probes as a server for clients that are observed at a non-global ip address (default: false)
	pub autonat_only_global_ips: bool,
}

impl Default for AutoNATConfig {
	fn default() -> Self {
		Self {
			autonat_retry_interval: Duration::from_secs(20),
			autonat_refresh_interval: Duration::from_secs(360),
			autonat_boot_delay: Duration::from_secs(5),
			autonat_throttle: Duration::from_secs(1),
			autonat_only_global_ips: false,
		}
	}
}

async fn build_swarm(
	cfg: &LibP2PConfig,
	version: &str,
	id_keys: &libp2p::identity::Keypair,
	kad_store: Store,
	is_ws_transport: bool,
) -> Result<Swarm<Behaviour>> {
	// create Identify Protocol Config
	let identify_cfg = identify::Config::new(IDENTITY_PROTOCOL.to_string(), id_keys.public())
		.with_agent_version(AgentVersion::new(version).to_string());

	// create AutoNAT Client Config
	let autonat_cfg = autonat::Config {
		retry_interval: cfg.autonat.autonat_retry_interval,
		refresh_interval: cfg.autonat.autonat_refresh_interval,
		boot_delay: cfg.autonat.autonat_boot_delay,
		throttle_server_period: cfg.autonat.autonat_throttle,
		only_global_ips: cfg.autonat.autonat_only_global_ips,
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
pub fn keypair(secret_key: &SecretKey) -> Result<identity::Keypair> {
	let keypair = match secret_key {
		// If seed is provided, generate secret key from seed
		SecretKey::Seed { seed } => {
			let seed_digest = multihash::Sha3_256::digest(seed.as_bytes());
			identity::Keypair::ed25519_from_bytes(seed_digest)
				.wrap_err("error generating secret key from seed")?
		},
		// Import secret key if provided
		SecretKey::Key { key } => {
			let mut decoded_key = [0u8; 32];
			hex::decode_to_slice(key.clone().into_bytes(), &mut decoded_key)
				.wrap_err("error decoding secret key from config")?;
			identity::Keypair::ed25519_from_bytes(decoded_key)
				.wrap_err("error importing secret key")?
		},
	};
	Ok(keypair)
}

// Returns [`true`] if the address appears to be globally reachable
// Take from the unstable std::net implementation
pub fn is_global(ip: Ipv4Addr) -> bool {
	!(ip.octets()[0] == 0
		|| ip.is_private()
		|| ip.is_loopback()
		|| ip.is_link_local()
		// addresses reserved for future protocols (`192.0.0.0/24`)
		// .9 and .10 are documented as globally reachable so they're excluded
		|| (
			ip.octets()[0] == 192 && ip.octets()[1] == 0 && ip.octets()[2] == 0
			&& ip.octets()[3] != 9 && ip.octets()[3] != 10
		)
		|| ip.is_documentation()
		|| ip.is_broadcast())
}

// Returns [`true`] if the multi-address IP appears to be globally reachable
pub fn is_multiaddr_global(address: &Multiaddr) -> bool {
	address
		.iter()
		.any(|protocol| matches!(protocol, libp2p::multiaddr::Protocol::Ip4(ip) if is_global(ip)))
}

#[cfg(test)]
mod tests {
	use super::*;
	use test_case::test_case;

	#[test_case("/ip4/159.73.143.3/tcp/37000" => true ; "Global IPv4")]
	#[test_case("/ip4/192.168.0.1/tcp/37000" => false ; "Local (192.168) IPv4")]
	#[test_case("/ip4/172.16.10.11/tcp/37000" => false ; "Local (172.16) IPv4")]
	#[test_case("/ip4/127.0.0.1/tcp/37000" => false ; "Loopback IPv4")]
	#[test_case("" => false ; "Empty multiaddr")]
	fn test_is_multiaddr_global(addr_str: &str) -> bool {
		let addr = if addr_str.is_empty() {
			Multiaddr::empty()
		} else {
			addr_str.parse().unwrap()
		};
		is_multiaddr_global(&addr)
	}
}
