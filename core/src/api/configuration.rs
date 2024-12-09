use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct APIConfig {
	/// Light client HTTP server host name (default: 127.0.0.1).
	pub http_server_host: String,
	/// Light client HTTP server port (default: 7007).
	pub http_server_port: u16,
}

impl Default for APIConfig {
	fn default() -> Self {
		Self {
			http_server_host: "127.0.0.1".to_owned(),
			http_server_port: 7007,
		}
	}
}

#[derive(Clone)]
pub struct SharedConfig {
	pub app_id: Option<u32>,
	pub confidence: f64,
	pub sync_start_block: Option<u32>,
	/// Wallet address of the user running the Light Client (LC)
	pub wallet_address: Option<String>
}

impl Default for SharedConfig {
	fn default() -> Self {
		Self {
			app_id: Default::default(),
			confidence: 99.9,
			sync_start_block: Default::default(),
			wallet_address: Some("0xDabc...ex455".to_string())
		}
	}
}
