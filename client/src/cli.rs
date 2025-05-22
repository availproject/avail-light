use avail_light_core::{network::Network, types::NetworkMode};
use clap::{command, ArgAction, Parser};
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
	/// Network mode: 'both' uses P2P and RPC, 'p2p_only' disables RPC, 'rpc_only' disables P2P
	#[arg(long, value_name = "MODE")]
	pub network_mode: Option<NetworkMode>,
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
	/// Address of the Light Client operator
	#[arg(long)]
	pub operator_address: Option<String>,
	/// Enable or disable light client tracking service
	#[arg(long, action = ArgAction::SetTrue)]
	pub tracking_service_enable: bool,
	/// Tracking service address
	#[arg(long)]
	pub tracking_service_address: Option<String>,
	/// Tracking service ping interval in seconds
	#[arg(long)]
	pub tracking_service_ping_interval: Option<u64>,
	#[arg(long, default_value = "false")]
	pub no_update: bool,
}
