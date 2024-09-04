use std::{ops::Range, time::Duration};

use avail_light_core::{
	api::configuration::{APIConfig, SharedConfig},
	network::{p2p::configuration::LibP2PConfig, rpc::configuration::RPCConfig},
	telemetry::otlp::OtelConfig,
	types::{
		option_duration_seconds_format, tracing_level_format, AppClientConfig, MaintenanceConfig,
		Origin, SyncClientConfig,
	},
};
use serde::{Deserialize, Serialize};
use tracing::Level;

/// Representation of a configuration used by this project.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct RuntimeConfig {
	/// Name of the project running the client. (default: "avail")
	pub project_name: String,
	#[serde(flatten)]
	pub api: APIConfig,
	#[serde(flatten)]
	pub libp2p: LibP2PConfig,
	#[serde(flatten)]
	pub rpc: RPCConfig,
	/// Genesis hash of the network to be connected to. Set to a string beginning with "DEV" to connect to any network.
	pub genesis_hash: String,
	/// If set, application client is started with given app_id (default: None).
	pub app_id: Option<u32>,
	/// Confidence threshold, used to calculate how many cells need to be sampled to achieve desired confidence (default: 92.0).
	pub confidence: f64,
	/// File system path where RocksDB used by light client, stores its data.
	pub avail_path: String,
	/// Log level, default is `INFO`. See `<https://docs.rs/log/0.4.14/log/enum.LevelFilter.html>` for possible log level values. (default: `INFO`).
	#[serde(with = "tracing_level_format")]
	pub log_level: Level,
	pub origin: Origin,
	/// If set to true, logs are displayed in JSON format, which is used for structured logging. Otherwise, plain text format is used (default: false).
	pub log_format_json: bool,
	#[serde(flatten)]
	pub otel: OtelConfig,
	pub total_memory_gb_threshold: f64,
	pub num_cpus_threshold: usize,
	/// Disables fetching of cells from RPC, set to true if client expects cells to be available in DHT (default: false).
	pub disable_rpc: bool,
	/// Number of seconds to postpone block processing after block finalized message arrives (default: 20).
	#[serde(with = "option_duration_seconds_format")]
	pub block_processing_delay: Option<Duration>,
	/// Fraction and number of the block matrix part to fetch (e.g. 2/20 means second 1/20 part of a matrix) (default: None)
	pub sync_start_block: Option<u32>,
	/// Enable or disable synchronizing finality. If disabled, finality is assumed to be verified until the starting block at the point the LC is started and is only checked for new blocks. (default: true)
	pub sync_finality_enable: bool,
	/// Threshold for the number of cells fetched via DHT for the app client (default: 5000)
	pub threshold: usize,
	/// Client alias for use in logs and metrics
	pub client_alias: Option<String>,
}

impl From<&RuntimeConfig> for SyncClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		SyncClientConfig {
			confidence: val.confidence,
			disable_rpc: val.disable_rpc,
			app_id: val.app_id,
		}
	}
}

impl From<&RuntimeConfig> for AppClientConfig {
	fn from(val: &RuntimeConfig) -> Self {
		AppClientConfig {
			disable_rpc: val.disable_rpc,
			threshold: val.threshold,
		}
	}
}

impl From<&RuntimeConfig> for MaintenanceConfig {
	fn from(val: &RuntimeConfig) -> Self {
		MaintenanceConfig {
			block_confidence_treshold: val.confidence,
			replication_factor: val.libp2p.kademlia.record_replication_factor.get() as u16,
			query_timeout: val.libp2p.kademlia.query_timeout,
			pruning_interval: val.libp2p.kademlia.store_pruning_interval,
			telemetry_flush_interval: val.otel.ot_flush_block_interval,
			automatic_server_mode: val.libp2p.kademlia.automatic_server_mode,
			total_memory_gb_threshold: val.total_memory_gb_threshold,
			num_cpus_threshold: val.num_cpus_threshold,
		}
	}
}

impl From<&RuntimeConfig> for SharedConfig {
	fn from(value: &RuntimeConfig) -> Self {
		Self {
			app_id: value.app_id,
			confidence: value.confidence,
			sync_start_block: value.sync_start_block,
		}
	}
}

impl Default for RuntimeConfig {
	fn default() -> Self {
		RuntimeConfig {
			project_name: "avail".to_string(),
			api: Default::default(),
			libp2p: Default::default(),
			rpc: Default::default(),
			genesis_hash: "DEV".to_owned(),
			app_id: None,
			confidence: 99.9,
			avail_path: "avail_path".to_owned(),
			log_level: Level::INFO,
			log_format_json: false,
			otel: Default::default(),
			total_memory_gb_threshold: 16.0,
			num_cpus_threshold: 4,
			disable_rpc: false,
			block_processing_delay: Some(Duration::from_secs(20)),
			sync_start_block: None,
			sync_finality_enable: false,
			threshold: 5000,
			origin: Origin::External,
			client_alias: None,
		}
	}
}

impl RuntimeConfig {
	/// A range bounded inclusively below and exclusively above
	pub fn sync_range(&self, end: u32) -> Range<u32> {
		let start = self.sync_start_block.unwrap_or(end);
		Range { start, end }
	}
}
