use allow_block_list::BlockedPeers;
use color_eyre::{eyre::WrapErr, Report, Result};
use configuration::LibP2PConfig;
use libp2p::{
	autonat, dcutr, identify,
	identity::{self, ed25519},
	kad::{self, PeerRecord},
	mdns, noise, ping, relay,
	swarm::NetworkBehaviour,
	tcp, upnp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use multihash::{self, Hasher};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{fmt, net::Ipv4Addr, str::FromStr};
use tokio::sync::oneshot;
use tracing::info;
#[cfg(feature = "network-analysis")]
pub mod analyzer;
mod client;
pub mod configuration;
mod event_loop;
mod kad_mem_providers;
#[cfg(not(feature = "kademlia-rocksdb"))]
mod kad_mem_store;
mod kad_rocksdb_store;
use crate::{
	data::{Database, P2PKeypairKey, RocksDB},
	types::SecretKey,
};
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
	Bootstrap(oneshot::Sender<Result<()>>),
}

type Command = Box<dyn FnOnce(&mut EventLoop) -> Result<(), Report> + Send>;

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

const KADEMLIA_PROTOCOL_BASE: &str = "/avail_kad/id/1.0.0";

fn protocol_name(genesis_hash: &str) -> libp2p::StreamProtocol {
	let mut gen_hash = genesis_hash.trim_start_matches("0x").to_string();
	gen_hash.truncate(6);

	libp2p::StreamProtocol::try_from_owned(format!("{KADEMLIA_PROTOCOL_BASE}-{gen_hash}",))
		.expect("Invalid Kademlia protocol name")
}

async fn build_swarm(
	cfg: &LibP2PConfig,
	version: &str,
	genesis_hash: &str,
	id_keys: &libp2p::identity::Keypair,
	kad_store: Store,
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

	let mut kad_cfg: kad::Config = cfg.into();
	kad_cfg.set_protocol_names(vec![protocol_name(genesis_hash)]);

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

	if cfg.ws_transport_enable {
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
		.set_mode(Some(cfg.kademlia.operation_mode.into()));

	Ok(swarm)
}

// Keypair function creates identity Keypair for a local node.
// From such generated keypair it derives multihash identifier of the local peer.
fn keypair(secret_key: &SecretKey) -> Result<identity::Keypair> {
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

fn get_or_init_keypair(cfg: &LibP2PConfig, db: RocksDB) -> Result<identity::Keypair> {
	if let Some(secret_key) = cfg.secret_key.as_ref() {
		return keypair(secret_key);
	};

	if let Some(mut bytes) = db.get(P2PKeypairKey) {
		return Ok(ed25519::Keypair::try_from_bytes(&mut bytes[..]).map(From::from)?);
	};

	let id_keys = identity::Keypair::generate_ed25519();
	let keypair = id_keys.clone().try_into_ed25519()?;
	db.put(P2PKeypairKey, keypair.to_bytes().to_vec());
	Ok(id_keys)
}

pub fn identity(cfg: &LibP2PConfig, db: RocksDB) -> Result<(identity::Keypair, PeerId)> {
	let keypair = get_or_init_keypair(cfg, db)?;
	let peer_id = PeerId::from(keypair.public());
	Ok((keypair, peer_id))
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
