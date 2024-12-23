use std::time::Duration;

use avail_rust::{self, Keypair};
use chrono::Utc;
use color_eyre::Result;
use libp2p::{Multiaddr, PeerId};
use serde::{Deserialize, Serialize};
use tokio::time;
use tracing::warn;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PingMessage {
	pub timestamp: i64,
	pub multiaddr: String,
	pub peer_id: String,
	pub block_number: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SignedPingMessage {
	pub message: PingMessage,
	pub signature: Vec<u8>,
	pub public_key: String,
}

pub fn create_and_sign_ping_message(
	keypair: &Keypair,
	multiaddr: &Multiaddr,
	peer_id: PeerId,
	block_number: u64,
) -> Result<(SignedPingMessage)> {
	let ping_message = PingMessage {
		timestamp: Utc::now().timestamp(),
		multiaddr: multiaddr.to_string(),
		peer_id: peer_id.to_string(),
		block_number: block_number.to_string(),
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

async fn run(
	interval: Duration,
	keypair: &Keypair,
	multiaddr: &Multiaddr,
	peer_id: PeerId,
	block_number: u64,
) {
	let mut interval = time::interval(interval);
	loop {
		interval.tick().await;
		match create_and_sign_ping_message(keypair, multiaddr, peer_id, block_number) {
			Ok(_) => {},
			Err(e) => {
				warn!("Error sending signed ping message: {}", e);
			},
		}
	}
}
