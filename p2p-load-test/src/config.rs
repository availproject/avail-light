use avail_light_core::network::{p2p::configuration::LibP2PConfig, Network};
use avail_light_core::types::{tracing_level_format, PeerAddress, SecretKey};
use clap::Parser;
use color_eyre::{eyre::eyre, Result};
use libp2p::{Multiaddr, PeerId};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::Level;

#[derive(Parser)]
#[command(version)]
pub struct CliOpts {
	/// Sets verbosity level.
	#[arg(long)]
	pub verbosity: Option<Level>,
	/// Sets logs format to JSON.
	#[arg(long)]
	pub logs_json: bool,
	/// Cleans DB state.
	#[arg(long)]
	pub clean: bool,
	/// Testnet or devnet selection.
	#[arg(short, long, value_name = "network", default_value = "hex")]
	pub network: Network,
	/// Seed string for libp2p keypair generation
	#[arg(long)]
	pub seed: Option<String>,
	/// P2P port
	#[arg(short, long)]
	pub p2p_port: Option<u16>,
	/// RocksDB store location
	#[arg(long, default_value = "./db")]
	pub db_path: String,
	/// Rate of DHT puts per second
	#[arg(long, default_value = "1")]
	pub rate: u64,
	/// Size of the data block in bytes (max 512KB)
	#[arg(long, default_value = "524288")]
	pub block_size: usize,
	/// Duration of the test in seconds (0 for infinite)
	#[arg(long, default_value = "0")]
	pub duration: u64,
	/// Number of unique blocks to cycle through
	#[arg(long, default_value = "10")]
	pub block_count: u32,
	/// Genesis hash of the network to connect to (overrides network default)
	#[arg(long)]
	pub genesis_hash: Option<String>,
	/// Comma-separated list of peer IDs to target for DHT puts (e.g., "12D3KooW...,12D3KooW...")
	#[arg(long)]
	pub target_peers: Option<String>,
	/// Comma-separated list of multiaddresses to add to the routing table (e.g., "/ip4/1.2.3.4/tcp/30333/p2p/12D3KooW...")
	#[arg(long)]
	pub manual_peers: Option<String>,
	/// Peer discovery interval in seconds
	#[arg(long, default_value = "10")]
	pub peer_discovery_interval: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
	/// Genesis hash of the network to be connected to.
	/// Set to "DEV" to connect to any network.
	pub genesis_hash: String,
	/// Log level.
	#[serde(with = "tracing_level_format")]
	pub log_level: Level,
	/// Log format: JSON for `true`, plain text for `false`.
	pub log_format_json: bool,
	/// Database file system path.
	pub db_path: String,
	#[serde(flatten)]
	pub libp2p: LibP2PConfig,
	/// Rate of DHT puts per second
	pub rate: u64,
	/// Size of the data block in bytes
	pub block_size: usize,
	/// Duration of the test in seconds (0 for infinite)
	pub duration: u64,
	/// Number of unique blocks to cycle through
	pub block_count: u32,
	/// Target peer IDs for DHT puts
	pub target_peers: Vec<PeerId>,
	/// Manual peers to add to routing table
	pub manual_peers: Vec<PeerAddress>,
	/// Peer discovery interval in seconds
	pub peer_discovery_interval: u64,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			genesis_hash: "DEV".to_owned(),
			log_level: Level::INFO,
			log_format_json: false,
			db_path: "./db".to_string(),
			libp2p: Default::default(),
			rate: 1,
			block_size: 524288, // 512KB
			duration: 0,
			block_count: 10,
			target_peers: vec![],
			manual_peers: vec![],
			peer_discovery_interval: 10,
		}
	}
}

pub fn load(opts: &CliOpts) -> Result<Config> {
	let mut config = Config::default();

	config.log_level = opts.verbosity.unwrap_or(config.log_level);
	config.log_format_json = opts.logs_json || config.log_format_json;

	let bootstrap = (
		opts.network.bootstrap_peer_id(),
		opts.network.bootstrap_multiaddr(),
	);
	config.libp2p.bootstraps = vec![PeerAddress::PeerIdAndMultiaddr(bootstrap)];
	config.genesis_hash = opts
		.genesis_hash
		.clone()
		.unwrap_or_else(|| opts.network.genesis_hash().to_string());

	if let Some(seed) = &opts.seed {
		config.libp2p.secret_key = Some(SecretKey::Seed {
			seed: seed.to_string(),
		})
	}

	if let Some(p2p_port) = opts.p2p_port {
		config.libp2p.port = p2p_port;
	}

	config.db_path = opts.db_path.to_string();
	config.rate = opts.rate;
	config.block_size = opts.block_size.min(524288); // Cap at 512KB
	config.duration = opts.duration;
	config.block_count = opts.block_count;
	config.peer_discovery_interval = opts.peer_discovery_interval;

	// Parse target peers
	if let Some(target_peers_str) = &opts.target_peers {
		config.target_peers = target_peers_str
			.split(',')
			.map(|s| s.trim())
			.filter(|s| !s.is_empty())
			.map(PeerId::from_str)
			.collect::<Result<Vec<_>, _>>()
			.map_err(|e| eyre!("Invalid peer ID in target_peers: {}", e))?;
	}

	// Parse manual peers
	if let Some(manual_peers_str) = &opts.manual_peers {
		config.manual_peers = manual_peers_str
			.split(',')
			.map(|s| s.trim())
			.filter(|s| !s.is_empty())
			.map(|s| {
				// First validate it's a proper multiaddress
				let addr = Multiaddr::from_str(s)
					.map_err(|e| eyre!("Invalid multiaddress '{}': {}", s, e))?;

				// Check if it contains a peer ID at the end
				let has_peer_id = addr
					.iter()
					.any(|p| matches!(p, libp2p::multiaddr::Protocol::P2p(_)));

				if !has_peer_id {
					return Err(eyre!(
						"Multiaddress must contain peer ID (e.g., /p2p/12D3KooW...): {}",
						s
					));
				}

				// Parse as PeerAddress::Compact
				Ok(PeerAddress::Compact(s.to_string().try_into()?))
			})
			.collect::<Result<Vec<_>>>()?;
	}

	if config.libp2p.bootstraps.is_empty() {
		return Err(eyre!("List of bootstraps must not be empty!"));
	}

	Ok(config)
}
