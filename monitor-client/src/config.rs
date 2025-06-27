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
	/// Time interval for bootstrap actions
	#[arg(long, default_value = "10")]
	pub bootstrap_interval: u64,
	/// Time interval for discovery actions
	#[arg(long, default_value = "10")]
	pub peer_discovery_interval: u64,
	/// Time interval for peer monitoring actions
	#[arg(long, default_value = "30")]
	pub peer_monitor_interval: u64,
	/// Seed string for libp2p keypair generation
	#[arg(long)]
	pub seed: Option<String>,
	/// P2P port
	#[arg(short, long)]
	pub p2p_port: Option<u16>,
	/// REST server port
	#[arg(short, long)]
	pub http_port: Option<u16>,
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
	#[arg(long, default_value = "10")]
	pub query_timeout: Option<u64>,
	#[arg(long, default_value = "3")]
	pub fail_threshold: usize,
	#[arg(long, default_value = "3")]
	pub success_threshold: usize,
	#[arg(long)]
	pub ot_collector_endpoint: Option<String>,
	#[arg(long)]
	pub ot_export_period: Option<u64>,
	#[arg(long)]
	pub ot_export_timeout: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct OtelConfig {
	pub ot_collector_endpoint: String,
	pub ot_export_period: u64,
	pub ot_export_timeout: u64,
}

impl Default for OtelConfig {
	fn default() -> Self {
		Self {
			ot_collector_endpoint: "http://127.0.0.1:4317".to_string(),
			ot_export_period: 5,
			ot_export_timeout: 10,
		}
	}
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct PaginationConfig {
	pub default_page: usize,
	pub default_limit: usize,
	pub min_page: usize,
	pub min_limit: usize,
	pub max_limit: usize,
}

impl Default for PaginationConfig {
	fn default() -> Self {
		Self {
			default_page: 1,
			default_limit: 50,
			min_page: 1,
			min_limit: 1,
			max_limit: 100,
		}
	}
}

impl PaginationConfig {
	pub fn validate_page(&self, page: usize) -> usize {
		page.max(self.min_page)
	}

	pub fn validate_limit(&self, limit: usize) -> usize {
		limit.clamp(self.min_limit, self.max_limit)
	}
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
	/// Genesis hash of the network to be connected to.
	/// Set to "DEV" to connect to any network.
	pub genesis_hash: String,
	/// Time interval for bootstrap actions.
	pub bootstrap_interval: u64,
	/// Time interval for peer discovery actions.
	pub peer_discovery_interval: u64,
	/// Time interval for peer monitor actions.
	pub peer_monitor_interval: u64,
	/// Log level.
	#[serde(with = "tracing_level_format")]
	pub log_level: Level,
	/// Log format: JSON for `true`, plain text for `false`.
	pub log_format_json: bool,
	/// Database file system path.
	pub db_path: String,
	#[serde(flatten)]
	pub libp2p: LibP2PConfig,
	pub fail_threshold: usize,
	pub success_threshold: usize,
	pub http_port: u16,
	pub pagination: PaginationConfig,
	pub otel: OtelConfig,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			genesis_hash: "DEV".to_owned(),
			log_level: Level::INFO,
			log_format_json: false,
			db_path: "./db".to_string(),
			libp2p: Default::default(),
			bootstrap_interval: 10,
			peer_discovery_interval: 10,
			peer_monitor_interval: 30,
			fail_threshold: 3,
			success_threshold: 3,
			http_port: 8090,
			pagination: PaginationConfig::default(),
			otel: OtelConfig::default(),
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

	if let Some(p2p_port) = opts.p2p_port {
		config.libp2p.port = p2p_port;
	}

	if let Some(http_port) = opts.http_port {
		config.http_port = http_port;
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

	config.bootstrap_interval = opts.bootstrap_interval;
	config.peer_discovery_interval = opts.peer_discovery_interval;
	config.peer_monitor_interval = opts.peer_monitor_interval;

	config.fail_threshold = opts.fail_threshold;
	config.success_threshold = opts.success_threshold;

	if config.libp2p.bootstraps.is_empty() {
		return Err(eyre!("List of bootstraps must not be empty!"));
	}
	if let Some(query_timeout) = opts.query_timeout {
		config.libp2p.kademlia.query_timeout = Duration::from_secs(query_timeout);
	}

	if let Some(endpoint) = &opts.ot_collector_endpoint {
		config.otel.ot_collector_endpoint = endpoint.clone();
	}
	if let Some(period) = opts.ot_export_period {
		config.otel.ot_export_period = period;
	}
	if let Some(timeout) = opts.ot_export_timeout {
		config.otel.ot_export_timeout = timeout;
	}
	Ok(config)
}
