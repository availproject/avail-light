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
