use crate::cli::CliOpts;
use avail_light_core::{
	network::{
		p2p::configuration::{
			AutoNATConfig, BehaviourConfig, IdentifyConfig, KademliaConfig, LibP2PConfig,
		},
		AutoNatMode,
	},
	telemetry::otlp::OtelConfig,
	types::{tracing_level_format, KademliaMode, Origin, ProjectName, SecretKey},
};
use color_eyre::eyre::{self, Context};
use serde::{Deserialize, Serialize};
use std::{fmt::Display, net::SocketAddr, num::NonZero, str::FromStr, time::Duration};
use tracing::Level;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct RuntimeConfig {
	/// Name of the project running the client. (default: "avail"). Project names are automatically converted to snake_case.
	/// Only alphanumeric characters and underscores are allowed.
	pub project_name: ProjectName,
	/// Bootstrap HTTP server host name (default: 127.0.0.1).
	pub http_server_host: String,
	/// Bootstrap HTTP server port (default: 7700).
	pub http_server_port: u16,
	/// Log level, default is `INFO`. See `<https://docs.rs/log/0.4.14/log/enum.LevelFilter.html>` for possible log level values. (default: `INFO`).
	#[serde(with = "tracing_level_format")]
	pub log_level: Level,
	/// If set to true, logs are displayed in JSON format, which is used for structured logging. Otherwise, plain text format is used (default: false).
	pub log_format_json: bool,
	#[serde(flatten)]
	pub libp2p: LibP2PConfig,
	#[serde(flatten)]
	pub otel: OtelConfig,
	/// Default bootstrap peerID is 12D3KooWStAKPADXqJ7cngPYXd2mSANpdgh1xQ34aouufHA2xShz
	pub origin: Origin,
	/// Genesis hash of the network to be connected to. Set to a string beginning with "DEV" to connect to any network.
	pub genesis_hash: String,
	/// Client alias for use in logs and metrics
	pub client_alias: Option<String>,
	/// File system path where RocksDB used by bootstrap client to stores its data.
	pub avail_bootstrap_path: String,
}

impl Default for RuntimeConfig {
	fn default() -> Self {
		RuntimeConfig {
			project_name: ProjectName::new("avail".to_string()),
			http_server_host: "127.0.0.1".to_owned(),
			http_server_port: 7700,
			log_level: Level::INFO,
			log_format_json: false,
			otel: Default::default(),
			origin: Origin::External,
			genesis_hash: "DEV".to_owned(),
			libp2p: LibP2PConfig {
				secret_key: Some(SecretKey::Seed {
					seed: "1".to_string(),
				}),
				port: 39000,
				webrtc_port: 39001,
				autonat: AutoNATConfig {
					throttle_clients_global_max: 120,
					throttle_clients_peer_max: 4,
					only_global_ips: false,
					..Default::default()
				},
				kademlia: KademliaConfig {
					query_timeout: Duration::from_secs(60),
					operation_mode: KademliaMode::Server,
					..Default::default()
				},
				identify: IdentifyConfig {
					agent_role: "bootstrap".to_string(),
					..Default::default()
				},
				behaviour: BehaviourConfig {
					auto_nat_mode: AutoNatMode::Enabled,
					..Default::default()
				},
				max_negotiating_inbound_streams: Some(20),
				task_command_buffer_size: Some(NonZero::new(30000).unwrap()),
				per_connection_event_buffer_size: Some(10000),
				dial_concurrency_factor: NonZero::new(5).unwrap(),
				..Default::default()
			},
			client_alias: None,
			avail_bootstrap_path: "avail_bootstrap_path".to_owned(),
		}
	}
}

impl RuntimeConfig {
	/// Applies bootstrap-specific overrides for libp2p fields that are NOT present in config file
	pub fn apply_defaults(&mut self, toml: &toml::Value) {
		let is_set = |key: &str| -> bool { toml.get(key).is_some() };

		let defaults = RuntimeConfig::default();

		if !is_set("port") {
			self.libp2p.port = defaults.libp2p.port;
		}
		if !is_set("webrtc_port") {
			self.libp2p.webrtc_port = defaults.libp2p.webrtc_port;
		}
		if !is_set("throttle_clients_global_max") {
			self.libp2p.autonat.throttle_clients_global_max =
				defaults.libp2p.autonat.throttle_clients_global_max;
		}
		if !is_set("throttle_clients_peer_max") {
			self.libp2p.autonat.throttle_clients_peer_max =
				defaults.libp2p.autonat.throttle_clients_peer_max;
		}
		if !is_set("only_global_ips") {
			self.libp2p.autonat.only_global_ips = defaults.libp2p.autonat.only_global_ips;
		}
		if !is_set("query_timeout") {
			self.libp2p.kademlia.query_timeout = defaults.libp2p.kademlia.query_timeout;
		}
		if !is_set("max_negotiating_inbound_streams") {
			self.libp2p.max_negotiating_inbound_streams =
				defaults.libp2p.max_negotiating_inbound_streams;
		}
		if !is_set("task_command_buffer_size") {
			self.libp2p.task_command_buffer_size = defaults.libp2p.task_command_buffer_size;
		}
		if !is_set("per_connection_event_buffer_size") {
			self.libp2p.per_connection_event_buffer_size =
				defaults.libp2p.per_connection_event_buffer_size;
		}
		if !is_set("dial_concurrency_factor") {
			self.libp2p.dial_concurrency_factor = defaults.libp2p.dial_concurrency_factor;
		}
		if !is_set("secret_key") {
			self.libp2p.secret_key = defaults.libp2p.secret_key;
		}
		if !is_set("agent_role") {
			self.libp2p.identify.agent_role = defaults.libp2p.identify.agent_role;
		}
		if !is_set("operation_mode") {
			self.libp2p.kademlia.operation_mode = defaults.libp2p.kademlia.operation_mode;
		}
		if !is_set("auto_nat_mode") {
			self.libp2p.behaviour.auto_nat_mode = defaults.libp2p.behaviour.auto_nat_mode;
		}
	}

	/// Applies CLI option overrides to the runtime configuration
	pub fn apply_opts(&mut self, opts: &CliOpts) {
		self.log_format_json = opts.logs_json || self.log_format_json;
		self.log_level = opts.verbosity.unwrap_or(self.log_level);

		if let Some(port) = opts.port {
			self.libp2p.port = port;
		}

		if let Some(http_port) = opts.http_server_port {
			self.http_server_port = http_port;
		}

		if let Some(http_host) = opts.http_server_host.clone() {
			self.http_server_host = http_host;
		}

		if let Some(secret_key) = &opts.private_key {
			self.libp2p.secret_key = Some(SecretKey::Key {
				key: secret_key.to_string(),
			});
		}

		if let Some(seed) = &opts.seed {
			self.libp2p.secret_key = Some(SecretKey::Seed {
				seed: seed.to_string(),
			});
		}

		if let Some(client_alias) = &opts.client_alias {
			self.client_alias = Some(client_alias.clone());
		}

		if opts.local_test_mode {
			self.libp2p.local_test_mode = true;
		}

		if let Some(address_blacklist) = &opts.address_blacklist {
			self.libp2p.address_blacklist = address_blacklist.clone();
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
