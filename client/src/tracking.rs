use std::{sync::Arc, time::Duration};

use avail_rust::{self, Keypair};
use chrono::Utc;
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use tokio::{sync::Mutex, time};
use tracing::warn;

use crate::ClientState;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PingMessage {
	pub timestamp: i64,
	pub multiaddr: String,
	pub peer_id: String,
	pub block_number: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SignedPingMessage {
	pub message: PingMessage,
	pub signature: Vec<u8>,
	pub public_key: String,
}

pub async fn create_and_sign_ping_message(
	keypair: Keypair,
	state: Arc<Mutex<ClientState>>,
) -> Result<SignedPingMessage> {
	let state = state.lock().await;
	let ping_message = PingMessage {
		timestamp: Utc::now().timestamp(),
		multiaddr: state.multiaddress.to_string(),
		peer_id: state.peer_id.to_string(),
		block_number: state.latest_block,
	};

	let message_bytes = serde_json::to_vec(&ping_message)?;

	let signature = keypair.sign(&message_bytes);

	let signed_message = SignedPingMessage {
		message: ping_message,
		signature: signature.0.to_vec(),
		public_key: keypair.public_key().to_account_id().to_string(),
	};
	Ok(signed_message)
}

pub async fn run(interval: Duration, keypair: Keypair, state: Arc<Mutex<ClientState>>) {
	let mut interval = time::interval(interval);
	loop {
		interval.tick().await;
		match create_and_sign_ping_message(keypair.clone(), state.clone()).await {
			Ok(_) => {},
			Err(e) => {
				warn!("Error sending signed ping message: {}", e);
			},
		}
	}
}
