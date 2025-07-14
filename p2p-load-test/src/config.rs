use avail_light_core::network::{p2p::configuration::LibP2PConfig, Network};
use avail_light_core::types::{tracing_level_format, PeerAddress, SecretKey};
use clap::Parser;
use color_eyre::{eyre::eyre, Result};
use serde::{Deserialize, Serialize};
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

	if config.libp2p.bootstraps.is_empty() {
		return Err(eyre!("List of bootstraps must not be empty!"));
	}

	Ok(config)
}
