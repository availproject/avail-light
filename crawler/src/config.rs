use avail_light_core::{
	network::p2p::configuration::LibP2PConfig, telemetry::otlp::OtelConfig,
	types::tracing_level_format,
};
use clap::{command, Parser};
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use std::fs;
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
}

#[derive(Debug, Serialize, Deserialize)]
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
	#[serde(flatten)]
	pub libp2p: LibP2PConfig,
	#[serde(flatten)]
	pub otel: OtelConfig,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			genesis_hash: "DEV".to_owned(),
			log_level: Level::INFO,
			log_format_json: false,
			avail_path: "avail_path".to_string(),
			client_alias: "crawler".to_string(),
			libp2p: Default::default(),
			otel: Default::default(),
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

	Ok(config)
}
