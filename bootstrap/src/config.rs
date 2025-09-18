use avail_light_core::types::{tracing_level_format, Origin, ProjectName, SecretKey};
use color_eyre::eyre::{self, Context, Result};
use libp2p::Multiaddr;
use serde::{Deserialize, Serialize};
use std::{fmt::Display, net::SocketAddr, str::FromStr};
use tracing::Level;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct RuntimeConfig {
	/// Name of the project running the client. (default: "avail"). Project names are automatically converted to snake_case.
	/// Only alphanumeric characters and underscores are allowed.
	pub project_name: ProjectName,
	/// Bootstrap HTTP server host name (default: 127.0.0.1).
	pub http_server_host: String,
	/// Bootstrap HTTP server port (default: 7900).
	pub http_server_port: u16,
	/// Log level, default is `INFO`. See `<https://docs.rs/log/0.4.14/log/enum.LevelFilter.html>` for possible log level values. (default: `INFO`).
	#[serde(with = "tracing_level_format")]
	pub log_level: Level,
	/// If set to true, logs are displayed in JSON format, which is used for structured logging. Otherwise, plain text format is used (default: false).
	pub log_format_json: bool,
	/// Default bootstrap peerID is 12D3KooWStAKPADXqJ7cngPYXd2mSANpdgh1xQ34aouufHA2xShz
	pub origin: Origin,
	/// Genesis hash of the network to be connected to. Set to a string beginning with "DEV" to connect to any network.
	pub genesis_hash: String,
	/// Client alias for use in logs and metrics
	pub client_alias: Option<String>,
	/// File system path where RocksDB used by bootstrap client to stores its data.
	pub avail_bootstrap_path: String,
	/// See [`avail_light_core::network::p2p::configuration::LibP2PConfig::port`] for usage (default: 39000).
	pub port: u16,
	/// See [`avail_light_core::network::p2p::configuration::LibP2PConfig::webrtc_port`] for usage (default: 39001).
	pub webrtc_port: u16,
	/// See [`avail_light_core::network::p2p::configuration::LibP2PConfig::secret_key`] for usage (default: { seed: "1" }).
	pub secret_key: Option<SecretKey>,
	/// See [`avail_light_core::telemetry::otlp::OtelConfig::ot_collector_endpoint`] for usage (default: "http://127.0.0.1:4317").
	pub ot_collector_endpoint: String,
	/// See [`avail_light_core::network::p2p::configuration::IdentifyConfig::hide_listen_addrs`] for usage (default: true).
	pub hide_listen_addrs: bool,
	/// See [`avail_light_core::network::p2p::configuration::LibP2PConfig::external_address`] for usage (default: None).
	pub external_address: Option<Multiaddr>,
}

impl RuntimeConfig {
	pub fn load(path: &str) -> Result<RuntimeConfig> {
		confy::load_path(path).wrap_err(format!("Failed to load configuration from: {path}"))
	}
}

impl Default for RuntimeConfig {
	fn default() -> Self {
		RuntimeConfig {
			project_name: ProjectName::new("avail".to_string()),
			http_server_host: "127.0.0.1".to_owned(),
			http_server_port: 7900,
			log_level: Level::INFO,
			log_format_json: false,
			origin: Origin::External,
			genesis_hash: "DEV".to_owned(),
			client_alias: None,
			avail_bootstrap_path: "avail_bootstrap_path".to_owned(),
			port: 39000,
			webrtc_port: 39001,
			secret_key: SecretKey::Seed {
				seed: "1".to_string(),
			}
			.into(),
			hide_listen_addrs: true,
			ot_collector_endpoint: "http://127.0.0.1:4317".to_string(),
			external_address: None,
		}
	}
}

pub struct Addr {
	pub host: String,
	pub port: u16,
}

impl From<&RuntimeConfig> for Addr {
	fn from(value: &RuntimeConfig) -> Self {
		Addr {
			host: value.http_server_host.clone(),
			port: value.http_server_port,
		}
	}
}

impl TryInto<SocketAddr> for Addr {
	type Error = eyre::Error;

	fn try_into(self) -> Result<SocketAddr, Self::Error> {
		SocketAddr::from_str(&format!("{self}")).wrap_err("Unable to parse host and port")
	}
}

impl Display for Addr {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}:{}", self.host, self.port)
	}
}
