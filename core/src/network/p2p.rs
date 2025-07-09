use allow_block_list::BlockedPeers;
use color_eyre::{
	eyre::{eyre, WrapErr},
	Report, Result,
};
use configuration::{
	auto_nat_client_config, identify_config, kad_config, AutoNatMode, LibP2PConfig,
};
use itertools::Either;
use libp2p::{
	autonat, identify,
	identity::{self, ed25519, Keypair},
	kad::{self, Mode, PeerRecord, QueryStats, Record, RecordKey},
	multiaddr::Protocol,
	ping,
	swarm::{behaviour::toggle::Toggle, NetworkBehaviour},
	Multiaddr, PeerId, Swarm, SwarmBuilder,
};
#[cfg(not(target_arch = "wasm32"))]
use libp2p::{noise, tcp, yamux};
#[cfg(not(target_arch = "wasm32"))]
use multihash::{self, Hasher};
use rand::rngs::OsRng;
use semver::Version;
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
use std::{fmt, net::Ipv4Addr, str::FromStr};
use tokio::sync::{
	mpsc::{self, UnboundedReceiver},
	oneshot,
};
#[cfg(target_arch = "wasm32")]
use tokio_with_wasm::alias as tokio;
use tracing::{info, warn};
#[cfg(target_arch = "wasm32")]
use web_time::Duration;

#[cfg(feature = "network-analysis")]
#[cfg(not(target_arch = "wasm32"))]
pub mod analyzer;
mod client;
pub mod configuration;
mod event_loop;
mod kad_mem_providers;
mod kad_mem_store;
#[cfg(feature = "rocksdb")]
mod kad_rocksdb_store;
#[cfg(not(target_arch = "wasm32"))]
pub mod restart;

pub use kad_mem_store::MemoryStoreConfig;
#[cfg(feature = "rocksdb")]
pub use kad_rocksdb_store::ExpirationCompactionFilterFactory;
#[cfg(feature = "rocksdb")]
pub use kad_rocksdb_store::RocksDBStoreConfig;

#[cfg(not(feature = "rocksdb"))]
pub type Store = kad_mem_store::MemoryStore;
#[cfg(feature = "rocksdb")]
pub type Store = kad_rocksdb_store::RocksDBStore;

use crate::{
	data::{Database, P2PKeypairKey},
	shutdown::Controller,
	types::{ProjectName, SecretKey},
};
pub use client::Client;
pub use event_loop::EventLoop;
pub use kad_mem_providers::ProvidersConfig;
use libp2p_allow_block_list as allow_block_list;
#[cfg(not(target_arch = "wasm32"))]
pub use restart::{forward_p2p_events, init_and_start_p2p_client, p2p_restart_manager};

const MINIMUM_SUPPORTED_BOOTSTRAP_VERSION: &str = "0.5.0";
const MINIMUM_SUPPORTED_LIGHT_CLIENT_VERSION: &str = "1.9.2";
pub const MINIMUM_P2P_CLIENT_RESTART_INTERVAL: u64 = 60; // seconds

pub const BOOTSTRAP_LIST_EMPTY_MESSAGE: &str = r#"
Bootstrap node list must not be empty.
Either use a '--network' flag or add a list of bootstrap nodes in the configuration file.
"#;

#[derive(Clone, Debug)]
pub enum OutputEvent {
	IncomingGetRecord,
	IncomingPutRecord,
	KadModeChange(Mode),
	Ping {
		peer: PeerId,
		rtt: Duration,
	},
	IncomingConnection,
	IncomingConnectionError,
	ExternalMultiaddressUpdate(Multiaddr),
	EstablishedConnection,
	OutgoingConnectionError,
	Count,
	PutRecord {
		block_num: u32,
		records: Vec<Record>,
	},
	PutRecordSuccess {
		record_key: RecordKey,
		query_stats: QueryStats,
	},
	PutRecordFailed {
		record_key: RecordKey,
		query_stats: QueryStats,
	},
	DiscoveredPeers {
		peers: Vec<(PeerId, Vec<Multiaddr>)>,
	},
	NewObservedAddress(Multiaddr),
}

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
	fn new(project_name: ProjectName, version: &str, cfg: &LibP2PConfig) -> Self {
		Self {
			base_version: format!("{project_name}-{}", cfg.identify.agent_base),
			role: cfg.identify.agent_role.clone(),
			release_version: version.to_string(),
			client_type: cfg.identify.agent_client_type.clone(),
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

pub type ClosestPeers = Vec<(PeerId, Vec<libp2p::Multiaddr>)>;

#[derive(Debug)]
pub enum QueryChannel {
	GetRecord(oneshot::Sender<Result<PeerRecord>>),
	GetClosestPeer(oneshot::Sender<Result<ClosestPeers>>),
}

type Command = Box<dyn FnOnce(&mut EventLoop) -> Result<(), Report> + Send>;

// Behaviour struct is used to derive delegated Libp2p behaviour implementation
#[derive(NetworkBehaviour)]
#[behaviour(event_process = false)]
pub struct ConfigurableBehaviour {
	#[behaviour(optional)]
	kademlia: Toggle<kad::Behaviour<Store>>,

	#[behaviour(optional)]
	identify: Toggle<identify::Behaviour>,

	#[behaviour(optional)]
	ping: Toggle<ping::Behaviour>,

	#[behaviour(optional)]
	auto_nat: Toggle<Either<autonat::v2::client::Behaviour, autonat::v2::server::Behaviour>>,

	#[behaviour(optional)]
	blocked_peers: Toggle<allow_block_list::Behaviour<BlockedPeers>>,
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

const KADEMLIA_PROTOCOL_BASE: &str = "/avail_kad/id/1.0.0";

fn protocol_name(genesis_hash: &str) -> libp2p::StreamProtocol {
	let mut gen_hash = genesis_hash.trim_start_matches("0x").to_string();
	gen_hash.truncate(6);

	libp2p::StreamProtocol::try_from_owned(format!("{KADEMLIA_PROTOCOL_BASE}-{gen_hash}",))
		.expect("Invalid Kademlia protocol name")
}

#[allow(clippy::too_many_arguments)]
pub async fn init(
	cfg: LibP2PConfig,
	project_name: ProjectName,
	id_keys: Keypair,
	version: &str,
	genesis_hash: &str,
	is_fat: bool,
	shutdown: Controller<String>,
	#[cfg(feature = "rocksdb")] db: crate::data::DB,
) -> Result<(Client, EventLoop, UnboundedReceiver<OutputEvent>)> {
	// create sender channel for P2P event loop commands
	let (command_sender, command_receiver) = mpsc::unbounded_channel();
	// create P2P Client
	let client = Client::new(
		command_sender,
		cfg.dht_parallelization_limit,
		cfg.kademlia.kad_record_ttl,
	);
	// create Store
	let store = Store::with_config(
		id_keys.public().to_peer_id(),
		(&cfg).into(),
		#[cfg(feature = "rocksdb")]
		db.inner(),
	);
	// create Swarm
	let swarm = build_swarm(&cfg, project_name, version, genesis_hash, &id_keys, store)
		.await
		.expect("Unable to build swarm.");
	let (event_sender, event_receiver) = mpsc::unbounded_channel();
	// create EventLoop
	let event_loop = EventLoop::new(cfg, swarm, is_fat, command_receiver, event_sender, shutdown);

	Ok((client, event_loop, event_receiver))
}

async fn build_swarm(
	cfg: &LibP2PConfig,
	project_name: ProjectName,
	version: &str,
	genesis_hash: &str,
	id_keys: &Keypair,
	kad_store: Store,
) -> Result<Swarm<ConfigurableBehaviour>> {
	// initialize SwarmBuilder
	#[cfg(not(target_arch = "wasm32"))]
	let tokio_swarm = SwarmBuilder::with_existing_identity(id_keys.clone()).with_tokio();
	#[cfg(target_arch = "wasm32")]
	let tokio_swarm = SwarmBuilder::with_existing_identity(id_keys.clone()).with_wasm_bindgen();

	// create behaviour closure that uses the configuration to
	// determine which components to enable
	let make_behaviour = move |key: &identity::Keypair| {
		let kademlia = if cfg.behaviour.enable_kademlia {
			let kad_cfg: kad::Config = kad_config(cfg, genesis_hash);
			Some(kad::Behaviour::with_config(
				key.public().to_peer_id(),
				kad_store,
				kad_cfg,
			))
		} else {
			None
		};

		let identify = if cfg.behaviour.enable_identify {
			// create Identify Protocol Config
			let identify_cfg = identify_config(cfg, id_keys.public(), project_name, version);
			Some(identify::Behaviour::new(identify_cfg))
		} else {
			None
		};

		let ping = if cfg.behaviour.enable_ping {
			Some(ping::Behaviour::new(ping::Config::new()))
		} else {
			None
		};

		let auto_nat = match cfg.behaviour.auto_nat_mode {
			AutoNatMode::Disabled => None,
			AutoNatMode::Server => Some(Either::Right(autonat::v2::server::Behaviour::new(OsRng))),
			AutoNatMode::Client => {
				let autonat_cfg = auto_nat_client_config(cfg);
				Some(Either::Left(autonat::v2::client::Behaviour::new(
					OsRng,
					autonat_cfg,
				)))
			},
		};

		let blocked_peers = if cfg.behaviour.enable_blocked_peers {
			Some(allow_block_list::Behaviour::default())
		} else {
			None
		};

		Ok(ConfigurableBehaviour {
			ping: ping.into(),
			identify: identify.into(),
			kademlia: kademlia.into(),
			auto_nat: auto_nat.into(),
			blocked_peers: blocked_peers.into(),
		})
	};

	// build swarm with appropriate transport
	let mut swarm;
	#[cfg(target_arch = "wasm32")]
	{
		use libp2p_webrtc_websys as webrtc;
		let builder = tokio_swarm
			.with_other_transport(|key| webrtc::Transport::new(webrtc::Config::new(&key)))?;

		swarm = builder
			.with_behaviour(make_behaviour)?
			.with_swarm_config(|c| generate_config(c, cfg))
			.build();
	}

	#[cfg(not(target_arch = "wasm32"))]
	if cfg.ws_transport_enable {
		let builder = tokio_swarm
			.with_websocket(noise::Config::new, yamux::Config::default)
			.await?;

		swarm = builder
			.with_behaviour(make_behaviour)?
			.with_swarm_config(|c| generate_config(c, cfg))
			.build();
	} else {
		let builder = tokio_swarm
			.with_tcp(
				tcp::Config::default().nodelay(false),
				noise::Config::new,
				yamux::Config::default,
			)?
			.with_dns()?;

		swarm = builder
			.with_behaviour(make_behaviour)?
			.with_swarm_config(|c| generate_config(c, cfg))
			.build();
	}

	info!("Local peerID: {}", swarm.local_peer_id());

	// Setting the mode this way disables automatic mode changes.
	//
	// Because the identify protocol doesn't allow us to change
	// agent data on the fly, we're forced to use static Kad modes
	// instead of relying on dynamic changes
	if let Some(kad) = swarm.behaviour_mut().kademlia.as_mut() {
		kad.set_mode(Some(cfg.kademlia.operation_mode.into()))
	}

	Ok(swarm)
}

// Keypair function creates identity Keypair for a local node.
// From such generated keypair it derives multihash identifier of the local peer.
fn keypair(secret_key: &SecretKey) -> Result<identity::Keypair> {
	let keypair = match secret_key {
		#[cfg(not(target_arch = "wasm32"))]
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

/// Checks for IPv4 addresses reserved for future protocols
fn is_reserved_ip(ip: &Ipv4Addr) -> bool {
	ip.octets()[0] == 192
		&& ip.octets()[1] == 0
		&& ip.octets()[2] == 0
		&& ip.octets()[3] != 9
		&& ip.octets()[3] != 10
}

/// Checks if the multi-address appears to be globally reachable
pub fn is_global_address(addr: &Multiaddr) -> bool {
	for protocol in addr.iter() {
		// Check private IPv4 ranges
		match protocol {
			Protocol::Ip4(ip) => {
				// Also includes loopback (127.0.0.0/8) and link-local (169.254.0.0/16)
				if !ip.is_private()
					&& !ip.is_loopback()
					&& !ip.is_link_local()
					&& !ip.is_documentation()
					&& !ip.is_broadcast()
					&& !is_reserved_ip(&ip)
				{
					return true;
				}
			},
			// Check private IPv6 ranges:
			Protocol::Ip6(ip) => {
				// Also check for loopback and link-local
				if !ip.is_unique_local() && !ip.is_loopback() && !ip.is_unicast_link_local() {
					return true;
				}
			},
			// Check DNS entries for localhost (case insensitive)
			Protocol::Dns(host)
			| Protocol::Dns4(host)
			| Protocol::Dns6(host)
			| Protocol::Dnsaddr(host) => {
				let host = host.to_lowercase();
				// Check for common private/local hostnames
				if host != "localhost" && !host.ends_with(".local") && !host.ends_with(".localhost")
				{
					return true;
				}
			},
			_ => continue,
		}
	}
	false
}

fn get_or_init_keypair(cfg: &LibP2PConfig, db: impl Database) -> Result<identity::Keypair> {
	// First, check if secret key is provided in config
	if let Some(secret_key) = cfg.secret_key.as_ref() {
		return keypair(secret_key);
	};

	// Try to retrieve the keypair from db
	if let Some(mut bytes) = db.get(P2PKeypairKey) {
		return Ok(ed25519::Keypair::try_from_bytes(&mut bytes[..]).map(From::from)?);
	}

	// Generate a new keypair if not found
	let id_keys = identity::Keypair::generate_ed25519();

	// Store the keypair in the db
	let keypair = id_keys.clone().try_into_ed25519()?;
	db.put(P2PKeypairKey, keypair.to_bytes().to_vec());

	Ok(id_keys)
}

pub fn identity(cfg: &LibP2PConfig, db: impl Database) -> Result<(identity::Keypair, PeerId)> {
	let keypair = get_or_init_keypair(cfg, db)?;
	let peer_id = PeerId::from(keypair.public());
	Ok((keypair, peer_id))
}

#[derive(PartialEq, Debug)]
pub enum DHTKey {
	Cell(u32, u32, u32),
	Row(u32, u32),
}

impl TryFrom<RecordKey> for DHTKey {
	type Error = color_eyre::Report;

	fn try_from(key: RecordKey) -> std::result::Result<Self, Self::Error> {
		match *String::from_utf8(key.to_vec())?
			.split(':')
			.map(str::parse::<u32>)
			.collect::<std::result::Result<Vec<_>, _>>()?
			.as_slice()
		{
			[block_num, row_num] => Ok(DHTKey::Row(block_num, row_num)),
			[block_num, row_num, col_num] => Ok(DHTKey::Cell(block_num, row_num, col_num)),
			_ => Err(eyre!("Invalid DHT key")),
		}
	}
}

pub fn extract_block_num(key: RecordKey) -> Result<u32> {
	key.try_into()
		.map(|dht_key| match dht_key {
			DHTKey::Cell(block_num, _, _) | DHTKey::Row(block_num, _) => block_num,
		})
		.map_err(|error| {
			warn!("Unable to cast KAD key to DHT key: {error}");
			eyre!("Invalid key: {error}")
		})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn dht_key_parse_record_key() {
		let row_key: DHTKey = RecordKey::new(&"1:2").try_into().unwrap();
		assert_eq!(row_key, DHTKey::Row(1, 2));

		let cell_key: DHTKey = RecordKey::new(&"3:2:1").try_into().unwrap();
		assert_eq!(cell_key, DHTKey::Cell(3, 2, 1));

		let result: Result<DHTKey> = RecordKey::new(&"1:2:4:3").try_into();
		_ = result.unwrap_err();

		let result: Result<DHTKey> = RecordKey::new(&"123").try_into();
		_ = result.unwrap_err();
	}

	#[test]
	fn test_ipv4_global() {
		// Test public IPv4 addresses that should be considered global
		let global_addrs = vec![
			"/ip4/8.8.8.8/tcp/53",         // Google DNS
			"/ip4/1.1.1.1/tcp/53",         // Cloudflare DNS
			"/ip4/208.67.222.222/tcp/53",  // OpenDNS
			"/ip4/172.15.255.255/tcp/80",  // Just outside private range
			"/ip4/172.32.0.1/tcp/80",      // Just outside private range
			"/ip4/9.255.255.255/tcp/80",   // Just outside private range
			"/ip4/11.0.0.1/tcp/80",        // Just outside private range
			"/ip4/192.167.255.255/tcp/80", // Just outside private range
			"/ip4/192.169.0.1/tcp/80",     // Just outside private range
			"/ip4/192.0.0.9/tcp/80",       // Globally reachable exception
			"/ip4/192.0.0.10/tcp/80",      // Globally reachable exception
		];

		for addr_str in global_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				is_global_address(&addr),
				"Failed to detect global IP: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_ipv4_non_global() {
		// Test private/local IPv4 addresses that should NOT be considered global
		let non_global_addrs = vec![
			// Private ranges
			"/ip4/10.0.0.1/tcp/8080",
			"/ip4/172.16.0.1/tcp/8080",
			"/ip4/192.168.1.1/tcp/8080",
			// Loopback
			"/ip4/127.0.0.1/tcp/8080",
			// Link-local
			"/ip4/169.254.1.1/tcp/8080",
			// Documentation
			"/ip4/192.0.2.1/tcp/8080",
			"/ip4/198.51.100.1/tcp/8080",
			"/ip4/203.0.113.1/tcp/8080",
			// Broadcast
			"/ip4/255.255.255.255/tcp/8080",
			// Reserved (except the globally reachable ones)
			"/ip4/192.0.0.1/tcp/8080",
			"/ip4/192.0.0.100/tcp/8080",
		];

		for addr_str in non_global_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				!is_global_address(&addr),
				"Incorrectly identified as global: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_ipv6_global() {
		// Test global IPv6 addresses
		let global_addrs = vec![
			"/ip6/2001:db8::1/tcp/8080", // Documentation range (but considered global by std lib)
			"/ip6/2001:4860:4860::8888/tcp/53", // Google DNS
			"/ip6/2606:4700:4700::1111/tcp/53", // Cloudflare DNS
			"/ip6/2001::/tcp/80",        // Global unicast
			"/ip6/ff02::1/tcp/8080",     // Multicast (global)
			"/ip6/fec0::1/tcp/8080",     // Just outside link-local range
			"/ip6/fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/tcp/8080", // Just outside unique local
			"/ip6/fe00::1/tcp/8080",     // Just outside unique local
		];

		for addr_str in global_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				is_global_address(&addr),
				"Failed to detect global IPv6: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_ipv6_non_global() {
		// Test non-global IPv6 addresses
		let non_global_addrs = vec![
			// Unique local addresses (fc00::/7)
			"/ip6/fc00::1/tcp/8080",
			"/ip6/fd00::1/tcp/8080",
			// Link-local addresses (fe80::/10)
			"/ip6/fe80::1/tcp/8080",
			"/ip6/fe80::dead:beef/tcp/8080",
			// Loopback address (::1)
			"/ip6/::1/tcp/8080",
		];

		for addr_str in non_global_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				!is_global_address(&addr),
				"Incorrectly identified as global: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_dns_global() {
		// Test global DNS addresses
		let global_dns_addrs = vec![
			"/dns/example.com/tcp/80",
			"/dns4/google.com/tcp/443",
			"/dns6/ipv6.google.com/tcp/443",
			"/dnsaddr/bootstrap.libp2p.io/tcp/443",
			"/dns/peer.example.org/tcp/4001",
			"/dns/github.com/tcp/443",
			"/dns/stackoverflow.com/tcp/443",
			"/dns/cloudflare.com/tcp/80",
		];

		for addr_str in global_dns_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				is_global_address(&addr),
				"Failed to detect global DNS: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_dns_non_global() {
		// Test non-global DNS addresses
		let non_global_dns_addrs = vec![
			"/dns/localhost/tcp/8080",
			"/dns4/localhost/tcp/8080",
			"/dns/LOCALHOST/tcp/8080", // Case insensitive
			"/dns/myserver.local/tcp/8080",
			"/dns/printer.local/tcp/631",
			"/dns/device.localhost/tcp/80",
			"/dns/test.localhost/tcp/443",
		];

		for addr_str in non_global_dns_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				!is_global_address(&addr),
				"Incorrectly identified as global: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_multiaddr_without_ip_or_dns() {
		// Test multiaddresses that don't contain IP or DNS addresses
		let non_ip_addrs = vec![
			"/tcp/8080",
			"/udp/53",
			"/p2p/QmYyQSo1c1Ym7orWxLYvCrM2EmxFTANf8wXmmE7DWjhx5N",
			"/memory/1234",
		];

		for addr_str in non_ip_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				!is_global_address(&addr),
				"Non-IP/DNS address incorrectly identified as global: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_mixed_protocols_global() {
		// Test that the function returns true if ANY protocol in the chain is global
		let mixed_addrs = vec![
			// Global IP first, then private DNS - should be global
			"/ip4/8.8.8.8/dns/localhost/tcp/80",
			// Private IP first, then global DNS - should be global
			"/ip4/192.168.1.1/dns/example.com/tcp/80",
			// Multiple global protocols - should be global
			"/ip4/1.1.1.1/dns/google.com/tcp/80",
		];

		for addr_str in mixed_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				is_global_address(&addr),
				"Mixed protocol address should be global: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_mixed_protocols_non_global() {
		// Test addresses where all IP/DNS protocols are non-global
		let non_global_addrs = vec![
			"/ip4/192.168.1.1/dns/localhost/tcp/80",
			"/ip4/10.0.0.1/dns/device.local/tcp/80",
			"/ip6/fc00::1/dns/test.localhost/tcp/80",
		];

		for addr_str in non_global_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				!is_global_address(&addr),
				"All-private protocol address should not be global: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_is_reserved_ip_integration_with_is_global() {
		use std::net::Ipv4Addr;

		// Test how reserved IPs work with is_global_address
		let test_cases = vec![
			("192.0.0.1", true, false),   // Reserved and should NOT be global
			("192.0.0.9", false, true),   // Not reserved and IS global
			("192.0.0.10", false, true),  // Not reserved and IS global
			("192.0.0.100", true, false), // Reserved and should NOT be global
		];

		for (ip_str, is_reserved_expected, should_be_global) in test_cases {
			let ip: Ipv4Addr = ip_str.parse().unwrap();
			let multiaddr_str = format!("/ip4/{}/tcp/80", ip_str);
			let addr = Multiaddr::from_str(&multiaddr_str).unwrap();

			// Test is_reserved_ip function
			assert_eq!(
				is_reserved_ip(&ip),
				is_reserved_expected,
				"is_reserved_ip failed for {}",
				ip_str
			);

			// Test is_global_address function
			assert_eq!(
				is_global_address(&addr),
				should_be_global,
				"is_global_address test failed for {}: expected {}, got {}",
				ip_str,
				should_be_global,
				is_global_address(&addr)
			);
		}
	}
}
