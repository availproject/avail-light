use std::fmt::{self, Display, Formatter};

use avail_light_core::types::block_matrix_partition_format;
use clap::{command, Parser, ValueEnum};
use kate_recovery::matrix::Partition;

#[derive(ValueEnum, Clone)]
pub enum Network {
	Local,
	Hex,
	Turing,
}

impl Network {
	pub fn bootstrap_peer_id(&self) -> &str {
		match self {
			Network::Local => "12D3KooWStAKPADXqJ7cngPYXd2mSANpdgh1xQ34aouufHA2xShz",
			Network::Hex => "12D3KooWBMwfo5qyoLQDRat86kFcGAiJ2yxKM63rXHMw2rDuNZMA",
			Network::Turing => "12D3KooWBkLsNGaD3SpMaRWtAmWVuiZg1afdNSPbtJ8M8r9ArGRT",
		}
	}

	pub fn bootstrap_multiaddrr(&self) -> &str {
		match self {
			Network::Local => "/ip4/127.0.0.1/tcp/39000",
			Network::Hex => "/dns/bootnode.1.lightclient.hex.avail.so/tcp/37000",
			Network::Turing => "/dns/bootnode.1.lightclient.turing.avail.so/tcp/37000",
		}
	}

	pub fn full_node_ws(&self) -> &str {
		match self {
			Network::Local => "ws://127.0.0.1:9944",
			Network::Hex => "wss://rpc-hex-devnet.avail.tools/ws",
			Network::Turing => "wss://turing-rpc.avail.so/ws",
		}
	}

	pub fn ot_collector_endpoint(&self) -> &str {
		match self {
			Network::Local => "http://127.0.0.1:4317",
			Network::Hex => "http://otel.lightclient.hex.avail.so:4317",
			Network::Turing => "http://otel.lightclient.turing.avail.so:4317",
		}
	}

	pub fn genesis_hash(&self) -> &str {
		match self {
			Network::Local => "DEV",
			Network::Hex => "9d5ea6a5d7631e13028b684a1a0078e3970caa78bd677eaecaf2160304f174fb",
			Network::Turing => "d3d2f3a3495dc597434a99d7d449ebad6616db45e4e4f178f31cc6fa14378b70",
		}
	}

	pub fn name(genesis_hash: &str) -> String {
		let network = match genesis_hash {
			"9d5ea6a5d7631e13028b684a1a0078e3970caa78bd677eaecaf2160304f174fb" => {
				Network::Hex.to_string()
			},
			"d3d2f3a3495dc597434a99d7d449ebad6616db45e4e4f178f31cc6fa14378b70" => {
				Network::Turing.to_string()
			},
			"DEV" => Network::Local.to_string(),
			_ => "other".to_string(),
		};

		let prefix = &genesis_hash[..std::cmp::min(6, genesis_hash.len())];
		format!("{}:{}", network, prefix)
	}
}

impl fmt::Display for Network {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match self {
			Network::Local => write!(f, "local"),
			Network::Hex => write!(f, "hex"),
			Network::Turing => write!(f, "turing"),
		}
	}
}

#[derive(ValueEnum, Clone)]
pub enum LogLevel {
	Info,
	Debug,
	Trace,
	Warn,
	Error,
}

impl Display for LogLevel {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.write_str(match self {
			LogLevel::Info => "INFO",
			LogLevel::Debug => "DEBUG",
			LogLevel::Trace => "TRACE",
			LogLevel::Warn => "WARN",
			LogLevel::Error => "ERROR",
		})
	}
}

#[derive(Parser)]
#[command(version)]
pub struct CliOpts {
	/// Path to the yaml configuration file
	#[arg(short, long, value_name = "FILE")]
	pub config: Option<String>,
	/// Path to the toml identity file
	#[arg(short, long, value_name = "FILE", default_value = "identity.toml")]
	pub identity: String,
	/// AppID for application client
	#[arg(long, value_name = "app-id")]
	pub app_id: Option<u32>,
	/// Testnet or devnet selection
	#[arg(short, long, value_name = "network")]
	pub network: Option<Network>,
	/// Run a clean light client, deleting existing avail_path folder
	#[arg(long)]
	pub clean: bool,
	/// Enable finality sync
	#[arg(short, long, value_name = "finality_sync_enable")]
	pub finality_sync_enable: bool,
	/// P2P port
	#[arg(short, long)]
	pub port: Option<u16>,
	/// HTTP port
	#[arg(long)]
	pub http_server_port: Option<u16>,
	/// Enable websocket transport
	#[arg(long, value_name = "ws_transport_enable")]
	pub ws_transport_enable: bool,
	/// Log level
	#[arg(long)]
	pub verbosity: Option<LogLevel>,
	// TODO: Deprecated since 1.9.0, remove it once it is safe
	/// Avail secret seed phrase password, overrides password from identity file
	#[arg(long)]
	pub avail_passphrase: Option<String>,
	/// Avail secret URI, overrides parameter from identity file
	#[arg(long)]
	pub avail_suri: Option<String>,
	/// Seed string for libp2p keypair generation
	#[arg(long)]
	pub seed: Option<String>,
	/// ed25519 private key for libp2p keypair generation
	#[arg(long)]
	pub private_key: Option<String>,
	/// fraction and number of the block matrix part to fetch (e.g. 2/20 means second 1/20 part of a matrix) (default: None)
	#[arg(long, value_parser = block_matrix_partition_format::parse)]
	pub block_matrix_partition: Option<Partition>,
	/// Set logs format to JSON
	#[arg(long)]
	pub logs_json: bool,
}
