use std::fs;

use avail_light_core::{
	network::Network,
	types::{MultiaddrConfig, RuntimeConfig, SecretKey},
};

use avail_light_core::network::p2p::BOOTSTRAP_LIST_EMPTY_MESSAGE;
use color_eyre::{eyre::Context, Result};

use clap::{command, Parser};
use color_eyre::eyre::eyre;
use tracing::Level;

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
	/// Path to the avail_path, where RocksDB stores its data
	#[arg(long)]
	pub avail_path: Option<String>,
	/// Enable finality sync
	#[arg(short, long, value_name = "finality_sync_enable")]
	pub finality_sync_enable: bool,
	/// P2P port
	#[arg(short, long)]
	pub port: Option<u16>,
	/// P2P WebRTC port
	#[arg(short, long)]
	pub webrtc_port: Option<u16>,
	/// HTTP port
	#[arg(long)]
	pub http_server_port: Option<u16>,
	/// Enable websocket transport
	#[arg(long, value_name = "ws_transport_enable")]
	pub ws_transport_enable: bool,
	/// Log level
	#[arg(long)]
	pub verbosity: Option<Level>,
	/// Avail secret seed phrase password
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
	/// Set logs format to JSON
	#[arg(long)]
	pub logs_json: bool,
	/// Set client alias for use in logs and metrics
	#[arg(long)]
	pub client_alias: Option<String>,
}

pub fn load_runtime_config(opts: &CliOpts) -> Result<RuntimeConfig> {
	let mut cfg = if let Some(config_path) = &opts.config {
		fs::metadata(config_path).map_err(|_| eyre!("Provided config file doesn't exist."))?;
		confy::load_path(config_path)
			.wrap_err(format!("Failed to load configuration from {}", config_path))?
	} else {
		RuntimeConfig::default()
	};

	cfg.log_format_json = opts.logs_json || cfg.log_format_json;
	cfg.log_level = opts.verbosity.unwrap_or(cfg.log_level);

	// Flags override the config parameters
	if let Some(network) = &opts.network {
		let bootstrap = (network.bootstrap_peer_id(), network.bootstrap_multiaddr());
		cfg.rpc.full_node_ws = network.full_node_ws();
		cfg.libp2p.bootstraps = vec![MultiaddrConfig::PeerIdAndMultiaddr(bootstrap)];
		cfg.otel.ot_collector_endpoint = network.ot_collector_endpoint().to_string();
		cfg.genesis_hash = network.genesis_hash().to_string();
	}

	if let Some(port) = opts.port {
		cfg.libp2p.port = port;
	}
	if let Some(http_port) = opts.http_server_port {
		cfg.api.http_server_port = http_port;
	}
	if let Some(webrtc_port) = opts.webrtc_port {
		cfg.libp2p.webrtc_port = webrtc_port;
	}
	if let Some(avail_path) = &opts.avail_path {
		cfg.avail_path = avail_path.to_string();
	}
	cfg.sync_finality_enable |= opts.finality_sync_enable;
	cfg.app_id = opts.app_id.or(cfg.app_id);
	cfg.libp2p.ws_transport_enable |= opts.ws_transport_enable;
	if let Some(secret_key) = &opts.private_key {
		cfg.libp2p.secret_key = Some(SecretKey::Key {
			key: secret_key.to_string(),
		});
	}

	if let Some(seed) = &opts.seed {
		cfg.libp2p.secret_key = Some(SecretKey::Seed {
			seed: seed.to_string(),
		})
	}

	if let Some(client_alias) = &opts.client_alias {
		cfg.client_alias = Some(client_alias.clone())
	}

	if cfg.libp2p.bootstraps.is_empty() {
		return Err(eyre!("{BOOTSTRAP_LIST_EMPTY_MESSAGE}"));
	}

	Ok(cfg)
}
