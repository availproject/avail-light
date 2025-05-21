use std::{fs, time::Duration};

use avail_light_core::{
	api::configuration::APIConfig,
	fat_client,
	network::{
		p2p::{configuration::LibP2PConfig, BOOTSTRAP_LIST_EMPTY_MESSAGE},
		rpc::configuration::RPCConfig,
		Network,
	},
	telemetry::otlp::OtelConfig,
	types::{
		block_matrix_partition_format, option_duration_seconds_format, tracing_level_format,
		NetworkMode, PeerAddress, SecretKey,
	},
};
use avail_rust::kate_recovery::matrix::Partition;
use clap::{command, Parser};
use color_eyre::{eyre::eyre, Result};
use serde::{Deserialize, Serialize};
use tracing::Level;

#[derive(Parser)]
#[command(version)]
pub struct CliOpts {
	/// Sets path to the yaml configuration file.
	#[arg(short, long, value_name = "FILE")]
	pub config: Option<String>,
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
	#[arg(short, long, value_name = "network")]
	pub network: Option<Network>,
	/// fraction and number of the block matrix part to fetch (e.g. 2/20 means second 1/20 part of a matrix) (default: None)
	#[arg(long, value_parser = block_matrix_partition_format::parse)]
	pub block_matrix_partition: Option<Partition>,
	/// Seed string for libp2p keypair generation
	#[arg(long)]
	pub seed: Option<String>,
	/// P2P port
	#[arg(short, long)]
	pub port: Option<u16>,
	/// Set client alias for use in logs and metrics
	#[arg(long)]
	pub client_alias: Option<String>,
	/// Path to the avail_path, where RocksDB stores its data
	#[arg(long)]
	pub avail_path: Option<String>,
	/// HTTP port
	#[arg(long)]
	pub http_server_port: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize)]
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
	pub avail_path: String,
	/// Client alias for use in logs and metrics.
	pub client_alias: String,
	/// Number of seconds to postpone block processing after block finalized message arrives (default: 0).
	#[serde(with = "option_duration_seconds_format")]
	pub block_processing_delay: Option<Duration>,
	/// Network mode determining which communication methods to use (default: Both).
	pub network_mode: NetworkMode,
	#[serde(flatten)]
	pub libp2p: LibP2PConfig,
	#[serde(flatten)]
	pub rpc: RPCConfig,
	#[serde(flatten)]
	pub otel: OtelConfig,
	#[serde(flatten)]
	pub fat: fat_client::Config,
	#[serde(flatten)]
	pub api: APIConfig,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			genesis_hash: "DEV".to_owned(),
			log_level: Level::INFO,
			log_format_json: false,
			avail_path: "avail_path".to_string(),
			client_alias: "fat".to_string(),
			network_mode: NetworkMode::P2POnly,
			libp2p: Default::default(),
			rpc: Default::default(),
			otel: Default::default(),
			fat: Default::default(),
			block_processing_delay: Some(Duration::from_secs(0)),
			api: Default::default(),
		}
	}
}

pub fn load(opts: &CliOpts) -> Result<Config> {
	let mut config = match &opts.config {
		Some(path) => {
			fs::metadata(path)?;
			confy::load_path(path)?
		},
		None => Config::default(),
	};

	config.log_level = opts.verbosity.unwrap_or(config.log_level);
	config.log_format_json = opts.logs_json || config.log_format_json;

	if let Some(network) = &opts.network {
		let bootstrap = (network.bootstrap_peer_id(), network.bootstrap_multiaddr());
		config.rpc.full_node_ws = network.full_node_ws();
		config.libp2p.bootstraps = vec![PeerAddress::PeerIdAndMultiaddr(bootstrap)];
		config.otel.ot_collector_endpoint = network.ot_collector_endpoint().to_string();
		config.genesis_hash = network.genesis_hash().to_string();
	}

	if let Some(seed) = &opts.seed {
		config.libp2p.secret_key = Some(SecretKey::Seed {
			seed: seed.to_string(),
		})
	}

	if let Some(port) = opts.port {
		config.libp2p.port = port;
	}

	if let Some(client_alias) = &opts.client_alias {
		config.client_alias = client_alias.clone()
	}

	if let Some(avail_path) = &opts.avail_path {
		config.avail_path = avail_path.to_string();
	}

	if let Some(http_port) = opts.http_server_port {
		config.api.http_server_port = http_port;
	}

	if let Some(http_port) = opts.http_server_port {
		config.api.http_server_port = http_port;
	}

	if config.libp2p.bootstraps.is_empty() {
		return Err(eyre!("{BOOTSTRAP_LIST_EMPTY_MESSAGE}"));
	}

	if let Some(partition) = &opts.block_matrix_partition {
		config.fat.block_matrix_partition = *partition
	}

	Ok(config)
}
