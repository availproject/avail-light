use avail_light_core::network::ServiceMode;
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
	/// P2P port
	#[arg(short, long)]
	pub port: Option<u16>,
	/// P2P WebRTC port
	#[arg(short, long)]
	pub webrtc_port: Option<u16>,
	/// HTTP port
	#[arg(long)]
	pub http_server_port: Option<u16>,
	#[arg(long)]
	pub http_server_host: Option<String>,
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

	/// AutoNAT behaviour mode: client, server, or disabled
	#[arg(long, value_enum, default_value = "server")]
	pub auto_nat_mode: ServiceMode,

	/// Relay behaviour mode: client, server, or disabled
	#[arg(long, value_enum, default_value = "disabled")]
	pub relay_mode: ServiceMode,

	/// Enable or disable DCUTR (Direct Connection Upgrade through Relay)
	/// Only works when relay_mode is Client or Both
	#[arg(
        long,
        default_value = "false",
        action = ArgAction::SetTrue,
        requires = "relay_mode",
        required_if_eq_any = [("relay_mode", "client"), ("relay_mode", "server")],
    )]
	pub enable_dcutr: bool,
}
