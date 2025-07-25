use super::{protocol_name, AgentVersion, ProvidersConfig};
use crate::network::p2p::MemoryStoreConfig;
use crate::network::ServiceMode;
use crate::types::{
	duration_seconds_format, option_duration_seconds_format, KademliaMode, PeerAddress,
	ProjectName, SecretKey,
};
use libp2p::{autonat, kad, multiaddr::Protocol, Multiaddr};
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
use std::{
	borrow::Cow,
	net::Ipv4Addr,
	num::{NonZeroU8, NonZeroUsize},
};
#[cfg(target_arch = "wasm32")]
use web_time::Duration;

/// Defines lip2p AutoNAT mode options
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum AutoNatMode {
	/// AutoNAT disabled
	Disabled,
	/// AutoNAT client only mode
	Client,
	/// AutoNAT server only mode
	Server,
}

impl From<ServiceMode> for AutoNatMode {
	fn from(mode: ServiceMode) -> Self {
		match mode {
			ServiceMode::Disabled => AutoNatMode::Disabled,
			ServiceMode::Client => AutoNatMode::Client,
			ServiceMode::Server => AutoNatMode::Server,
		}
	}
}

/// Define a configuration struct for Behaviour components
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BehaviourConfig {
	pub enable_kademlia: bool,
	pub enable_identify: bool,
	pub enable_ping: bool,
	pub auto_nat_mode: AutoNatMode,
	pub enable_blocked_peers: bool,
}

impl Default for BehaviourConfig {
	fn default() -> Self {
		Self {
			enable_kademlia: true,
			enable_identify: true,
			enable_ping: true,
			auto_nat_mode: AutoNatMode::Client,
			enable_blocked_peers: true,
		}
	}
}

/// Libp2p AutoNAT configuration (see [RuntimeConfig] for details)
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct AutoNATConfig {
	/// Client configuration:
	/// Interval in which the NAT status should be re-tried if it is currently unknown or max confidence was not reached yet. (default: 90 sec)
	#[serde(with = "duration_seconds_format")]
	pub retry_interval: Duration,
	/// Interval in which the NAT should be tested again if max confidence was reached in a status. (default: 900 sec)
	#[serde(with = "duration_seconds_format")]
	pub refresh_interval: Duration,
	/// AutoNat on init delay before starting the fist probe. (default: 15 sec)
	#[serde(with = "duration_seconds_format")]
	pub boot_delay: Duration,
	/// AutoNat throttle period for re-using a peer as server for a dial-request. (default: 90 sec)
	#[serde(with = "duration_seconds_format")]
	pub throttle: Duration,
	/// Configures AutoNAT behaviour to reject probes as a server for clients that are observed at a non-global ip address (default: true)
	pub only_global_ips: bool,
	/// Max total dial requests done for the throttle period. (default: 30)
	pub throttle_clients_global_max: usize,
	/// Max dial requests done in throttle period for a peer. (default: 3)
	pub throttle_clients_peer_max: usize,
	/// Period for throttling clients requests. (default: 1 sec)
	#[serde(with = "duration_seconds_format")]
	pub throttle_clients_period: Duration,
}

impl Default for AutoNATConfig {
	fn default() -> Self {
		Self {
			retry_interval: Duration::from_secs(90),
			refresh_interval: Duration::from_secs(15 * 60),
			boot_delay: Duration::from_secs(15),
			throttle: Duration::from_secs(90),
			only_global_ips: true,
			throttle_clients_global_max: 30,
			throttle_clients_peer_max: 3,
			throttle_clients_period: Duration::from_secs(1),
		}
	}
}

/// Identify configuration for libp2p identify protocol
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct IdentifyConfig {
	/// Protocol name/version to use for the identify protocol (default: "/avail/light/1.0.0")
	pub protocol_name: String,
	/// Base name for the agent version string (default: "light-client")
	pub agent_base: String,
	/// Role identifier for the agent version string (default: "light-client")
	pub agent_role: String,
	/// Client type for the agent version string (default: "rust-client")
	pub agent_client_type: String,
	/// Cache size for peer addresses (default: 100)
	pub cache_size: Option<usize>,
	/// The delay between identification requests (default: 10 min)
	#[serde(with = "duration_seconds_format")]
	pub interval: Duration,
	/// Whether to push listen address updates to connected peers (default: false)
	pub push_listen_addr_updates: bool,
	/// Whether to include our listen addresses in our responses (default: false)
	pub hide_listen_addrs: bool,
}

impl Default for IdentifyConfig {
	fn default() -> Self {
		Self {
			protocol_name: "/avail/light/1.0.0".to_string(),
			agent_base: "light-client".to_string(),
			agent_role: "light-client".to_string(),
			agent_client_type: "rust-client".to_string(),
			cache_size: Some(100),
			interval: Duration::from_secs(10 * 60),
			push_listen_addr_updates: false,
			hide_listen_addrs: true,
		}
	}
}

/// Kademlia configuration (see [RuntimeConfig] for details)
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct KademliaConfig {
	/// Kademlia configuration - WARNING: Changing the default values might cause the peer to suffer poor performance!
	/// Default Kademlia config values have been copied from rust-libp2p Kademila defaults
	///
	/// Time-to-live for DHT entries in seconds (default: 24h).
	/// Default value is set for light clients. Due to the heavy duty nature of the fat clients, it is recommended to be set far bellow this
	/// value - not greater than 1hr.
	/// Record TTL, publication and replication intervals are co-dependent, meaning that TTL >> publication_interval >> replication_interval.
	#[serde(with = "duration_seconds_format")]
	pub kad_record_ttl: Duration,
	/// Defines a period of time in which periodic bootstraps will be repeated in seconds. (default: 5 min)
	#[serde(with = "duration_seconds_format")]
	pub bootstrap_period: Duration,
	/// Sets the (re-)publication interval of stored records in seconds. (default: None).
	/// Default value is set for light clients. Fat client value needs to be inferred from the TTL value.
	/// This interval should be significantly shorter than the record TTL, to ensure records do not expire prematurely.
	#[serde(with = "option_duration_seconds_format")]
	pub publication_interval: Option<Duration>,
	/// Sets the (re-)replication interval for stored records in seconds. (default: None).
	/// Default value is set for light clients. Fat client value needs to be inferred from the TTL and publication interval values.
	/// This interval should be significantly shorter than the publication interval, to ensure persistence between re-publications.
	#[serde(with = "option_duration_seconds_format")]
	pub record_replication_interval: Option<Duration>,
	/// The replication factor determines to how many closest peers a record is replicated. (default: 20).
	pub record_replication_factor: NonZeroUsize,
	/// Sets the Kademlia record store pruning interval in blocks (default: 180).
	pub store_pruning_interval: u32,
	/// Sets the timeout for a single Kademlia query. (default: 10s).
	#[serde(with = "duration_seconds_format")]
	pub query_timeout: Duration,
	/// Sets the allowed level of parallelism for iterative Kademlia queries. (default: 3).
	pub query_parallelism: NonZeroUsize,
	/// Sets the Kademlia caching strategy to use for successful lookups. (default: 1).
	/// If set to 0, caching is disabled.
	pub caching_max_peers: u16,
	/// Require iterative queries to use disjoint paths for increased resiliency in the presence of potentially adversarial nodes. (default: false).
	pub disjoint_query_paths: bool,
	/// The maximum number of records in the memory store. (default: 2400000)
	/// The default value has been calculated to sustain ~1hr worth of cells, in case of blocks with max sizes being produces in 20s block time for fat clients
	/// (256x512) * 3 * 60
	pub max_kad_record_number: usize,
	/// The maximum size of record values, in bytes. (default: 8192).
	pub max_kad_record_size: usize,
	/// The maximum number of provider records for which the local node is the provider. (default: 1024).
	pub max_kad_provided_keys: usize,
	/// Sets Kademlia mode (server/client, default client)
	pub operation_mode: KademliaMode,
	/// Sets the automatic Kademlia server mode switch (default: true)
	pub automatic_server_mode: bool,
	/// Sets the timeout duration after which a pending entry becomes eligible for insertion on a full bucket. (default: 60s)
	#[serde(with = "duration_seconds_format")]
	pub kbucket_pending_timeout: Duration,
}

impl Default for KademliaConfig {
	fn default() -> Self {
		Self {
			kad_record_ttl: Duration::from_secs(24 * 60 * 60),
			bootstrap_period: Duration::from_secs(5 * 60),
			publication_interval: None,
			record_replication_interval: None,
			record_replication_factor: NonZeroUsize::new(5).unwrap(),
			store_pruning_interval: 180,
			query_timeout: Duration::from_secs(10),
			query_parallelism: NonZeroUsize::new(3).unwrap(),
			caching_max_peers: 1,
			disjoint_query_paths: false,
			max_kad_record_number: 2400000,
			max_kad_record_size: 8192 * 2, // Maximum for 512 columns
			max_kad_provided_keys: 1024,
			operation_mode: KademliaMode::Client,
			automatic_server_mode: true,
			kbucket_pending_timeout: Duration::from_secs(60),
		}
	}
}

#[derive(Clone, Serialize, Debug, Deserialize)]
#[serde(default)]
pub struct LibP2PConfig {
	/// Secret key for libp2p keypair. Can be either set to `seed` or to `key`.
	/// If set to seed, keypair will be generated from that seed.
	/// If set to key, a valid ed25519 private key must be provided, else the client will fail
	/// If `secret_key` is not set, random seed will be used.
	pub secret_key: Option<SecretKey>,
	/// P2P TCP listener port (default: 37000).
	pub port: u16,
	/// P2P WebRTC listener port (default: 37001).
	pub webrtc_port: u16,
	/// P2P WebSocket switch. Note: it's mutually exclusive with the TCP listener (default: false)
	pub ws_transport_enable: bool,
	/// AutoNAT configuration
	#[serde(flatten)]
	pub autonat: AutoNATConfig,
	/// Kademlia configuration
	#[serde(flatten)]
	pub kademlia: KademliaConfig,
	/// Identify configuration
	#[serde(flatten)]
	pub identify: IdentifyConfig,
	/// Swarm behaviour config
	#[serde(flatten)]
	pub behaviour: BehaviourConfig,
	/// Sets the amount of time to keep connections alive when they're idle. (default: 10s).
	#[serde(with = "duration_seconds_format")]
	pub connection_idle_timeout: Duration,
	pub max_negotiating_inbound_streams: Option<usize>,
	pub task_command_buffer_size: Option<NonZeroUsize>,
	pub per_connection_event_buffer_size: Option<usize>,
	pub dial_concurrency_factor: NonZeroU8,
	/// Vector of Light Client bootstrap nodes, used to bootstrap DHT. If not set, light client acts as a bootstrap node, waiting for first peer to connect for DHT bootstrap (default: empty).
	pub bootstraps: Vec<PeerAddress>,
	/// Maximum number of parallel tasks spawned for GET and PUT operations on DHT (default: 20).
	pub dht_parallelization_limit: usize,
	/// Interval between requests in ping protocol (default: 60s).
	#[serde(with = "duration_seconds_format")]
	pub ping_interval: Duration,
	/// Local test mode flag, currently only disables the private address filtering for KAD discovery and allows manual address confirmation of local addresses.
	pub local_test_mode: bool,
}

impl Default for LibP2PConfig {
	fn default() -> Self {
		Self {
			secret_key: None,
			port: 37000,
			webrtc_port: 37001,
			ws_transport_enable: false,
			autonat: Default::default(),
			kademlia: Default::default(),
			identify: Default::default(),
			behaviour: Default::default(),
			connection_idle_timeout: Duration::from_secs(10),
			max_negotiating_inbound_streams: None,
			task_command_buffer_size: None,
			per_connection_event_buffer_size: None,
			dial_concurrency_factor: NonZeroU8::new(8).unwrap(),
			bootstraps: vec![],
			dht_parallelization_limit: 20,
			ping_interval: Duration::from_secs(60),
			local_test_mode: false,
		}
	}
}

impl LibP2PConfig {
	pub fn tcp_multiaddress(&self) -> Multiaddr {
		let tcp_multiaddress = Multiaddr::empty()
			.with(Protocol::from(Ipv4Addr::UNSPECIFIED))
			.with(Protocol::Tcp(self.port));

		if self.ws_transport_enable {
			tcp_multiaddress.with(Protocol::Ws(Cow::Borrowed("avail-light")))
		} else {
			tcp_multiaddress
		}
	}

	pub fn webrtc_multiaddress(&self) -> Multiaddr {
		Multiaddr::from(Ipv4Addr::UNSPECIFIED)
			.with(Protocol::Udp(self.webrtc_port))
			.with(Protocol::WebRTCDirect)
	}

	pub fn effective_max_negotiating_inbound_streams(&self) -> usize {
		self.max_negotiating_inbound_streams
			.unwrap_or(match self.kademlia.operation_mode {
				KademliaMode::Client => 128,
				KademliaMode::Server => 10000,
			})
	}

	pub fn effective_task_command_buffer_size(&self) -> NonZeroUsize {
		self.task_command_buffer_size
			.unwrap_or_else(|| match self.kademlia.operation_mode {
				KademliaMode::Client => NonZeroUsize::new(32).unwrap(),
				KademliaMode::Server => NonZeroUsize::new(30000).unwrap(),
			})
	}

	pub fn effective_per_connection_event_buffer_size(&self) -> usize {
		self.per_connection_event_buffer_size.unwrap_or({
			match self.kademlia.operation_mode {
				KademliaMode::Client => 7,
				KademliaMode::Server => 10000,
			}
		})
	}
}

/// Creates Identify Config
pub fn identify_config(
	cfg: &LibP2PConfig,
	public_key: libp2p::identity::PublicKey,
	project_name: ProjectName,
	version: &str,
) -> libp2p::identify::Config {
	let mut identify_cfg =
		libp2p::identify::Config::new(cfg.identify.protocol_name.clone(), public_key)
			.with_interval(cfg.identify.interval)
			.with_push_listen_addr_updates(cfg.identify.push_listen_addr_updates)
			.with_hide_listen_addrs(cfg.identify.hide_listen_addrs)
			.with_agent_version(AgentVersion::new(project_name, version, cfg).to_string());

	if let Some(cache_size) = cfg.identify.cache_size {
		identify_cfg = identify_cfg.with_cache_size(cache_size);
	}

	identify_cfg
}

/// Creates AutoNAT Config
pub fn auto_nat_config(cfg: &LibP2PConfig) -> autonat::Config {
	autonat::Config {
		retry_interval: cfg.autonat.retry_interval,
		refresh_interval: cfg.autonat.refresh_interval,
		boot_delay: cfg.autonat.boot_delay,
		throttle_server_period: cfg.autonat.throttle,
		only_global_ips: cfg.autonat.only_global_ips,
		..Default::default()
	}
}

/// Creates Kademlia Config
pub fn kad_config(cfg: &LibP2PConfig, genesis_hash: &str) -> kad::Config {
	let mut kad_cfg = kad::Config::new(protocol_name(genesis_hash));
	kad_cfg
		.set_replication_factor(cfg.kademlia.record_replication_factor)
		.set_query_timeout(cfg.kademlia.query_timeout)
		.set_parallelism(cfg.kademlia.query_parallelism)
		.set_caching(kad::Caching::Enabled {
			max_peers: cfg.kademlia.caching_max_peers,
		})
		.disjoint_query_paths(cfg.kademlia.disjoint_query_paths)
		.set_record_filtering(kad::StoreInserts::FilterBoth)
		.set_periodic_bootstrap_interval(Some(cfg.kademlia.bootstrap_period))
		.set_kbucket_pending_timeout(cfg.kademlia.kbucket_pending_timeout)
		.set_publication_interval(cfg.kademlia.publication_interval)
		.set_replication_interval(cfg.kademlia.record_replication_interval);
	kad_cfg
}

impl From<&LibP2PConfig> for MemoryStoreConfig {
	fn from(cfg: &LibP2PConfig) -> Self {
		MemoryStoreConfig {
			max_records: cfg.kademlia.max_kad_record_number, // ~2hrs
			max_value_bytes: cfg.kademlia.max_kad_record_size + 1,
			providers: ProvidersConfig {
				max_providers_per_key: usize::from(cfg.kademlia.record_replication_factor), // Needs to match the replication factor, per libp2p docs
				max_provided_keys: cfg.kademlia.max_kad_provided_keys,
			},
		}
	}
}

#[cfg(feature = "rocksdb")]
impl From<&LibP2PConfig> for super::RocksDBStoreConfig {
	fn from(cfg: &LibP2PConfig) -> Self {
		super::RocksDBStoreConfig {
			max_value_bytes: cfg.kademlia.max_kad_record_size + 1,
			providers: ProvidersConfig {
				max_providers_per_key: usize::from(cfg.kademlia.record_replication_factor), // Needs to match the replication factor, per libp2p docs
				max_provided_keys: cfg.kademlia.max_kad_provided_keys,
			},
		}
	}
}
