use std::num::NonZeroUsize;
use std::time::Duration;

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
	/// Time interval for monitoring actions
	#[arg(long, default_value = "10")]
	pub interval: u64,
	/// Seed string for libp2p keypair generation
	#[arg(long)]
	pub seed: Option<String>,
	/// P2P port
	#[arg(short, long)]
	pub port: Option<u16>,
	/// RocksDB store location
	#[arg(long, default_value = "./db")]
	pub db_path: String,
	#[arg(long, default_value = "5")]
	pub connection_idle_timeout: Option<u64>,
	#[arg(long, default_value = "10000")]
	pub max_negotiating_inbound_streams: Option<usize>,
	#[arg(long, default_value = "30000")]
	pub task_command_buffer_size: Option<usize>,
	#[arg(long, default_value = "10000")]
	pub per_connection_event_buffer_size: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
	/// Genesis hash of the network to be connected to.
	/// Set to "DEV" to connect to any network.
	pub genesis_hash: String,
	/// Time interval for monitoring actions.
	pub interval: u64,
	/// Log level.
	#[serde(with = "tracing_level_format")]
	pub log_level: Level,
	/// Log format: JSON for `true`, plain text for `false`.
	pub log_format_json: bool,
	/// Database file system path.
	pub db_path: String,
	#[serde(flatten)]
	pub libp2p: LibP2PConfig,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			genesis_hash: "DEV".to_owned(),
			log_level: Level::INFO,
			log_format_json: false,
			db_path: "./db".to_string(),
			libp2p: Default::default(),
			interval: 10,
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
	config.genesis_hash = opts.network.genesis_hash().to_string();

	if let Some(seed) = &opts.seed {
		config.libp2p.secret_key = Some(SecretKey::Seed {
			seed: seed.to_string(),
		})
	}

	if let Some(port) = opts.port {
		config.libp2p.port = port;
	}

	if let Some(connection_idle_timeout) = opts.connection_idle_timeout {
		config.libp2p.connection_idle_timeout = Duration::from_secs(connection_idle_timeout);
	}

	if let Some(max_negotiating_inbound_streams) = opts.max_negotiating_inbound_streams {
		config.libp2p.max_negotiating_inbound_streams = max_negotiating_inbound_streams;
	}
	if let Some(task_command_buffer_size) = opts.task_command_buffer_size {
		config.libp2p.task_command_buffer_size =
			NonZeroUsize::new(task_command_buffer_size).unwrap();
	}

	if let Some(per_connection_event_buffer_size) = opts.per_connection_event_buffer_size {
		config.libp2p.per_connection_event_buffer_size = per_connection_event_buffer_size;
	}

	config.db_path = opts.db_path.to_string();
	config.interval = opts.interval;

	if config.libp2p.bootstraps.is_empty() {
		return Err(eyre!("List of bootstraps must not be empty!"));
	}
	Ok(config)
}
