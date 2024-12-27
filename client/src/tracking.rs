use std::{sync::Arc, time::Duration};

use avail_rust::{self, Keypair};
use chrono::Utc;
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use tokio::{sync::Mutex, time};
use tracing::{trace, warn};

use crate::TrackingState;

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

pub async fn create_and_sign_ping_message(
	keypair: Keypair,
	tracking_state: Arc<Mutex<TrackingState>>,
) -> Result<SignedPingMessage> {
	let state = tracking_state.lock().await;
	let ping_message = PingMessage {
		timestamp: Utc::now().timestamp(),
		multiaddr: state.multiaddress.to_string(),
		peer_id: state.peer_id.to_string(),
		block_number: state.latest_block.to_string(),
	};
	drop(state);
	let message_bytes = serde_json::to_vec(&ping_message)?;

	let signature = keypair.sign(&message_bytes);

	let signed_message = SignedPingMessage {
		message: ping_message,
		signature: signature.0.to_vec(),
		public_key: keypair.public_key().to_account_id().to_string(),
	};
	Ok(signed_message)
}

pub async fn run(
	tracking_interval: Duration,
	keypair: Keypair,
	tracking_state: Arc<Mutex<TrackingState>>,
	tracker_address: String,
) {
	let mut interval = time::interval(tracking_interval);
	loop {
		interval.tick().await;
		match create_and_sign_ping_message(keypair.clone(), tracking_state.clone()).await {
			Ok(signed_ping_message) => {
				let client = reqwest::Client::new();
				match client
					.post(tracker_address.clone())
					.json(&signed_ping_message)
					.timeout(tracking_interval)
					.send()
					.await
				{
					Ok(res) => trace!("Signed ping message sent. Response: {:?}", res),
					Err(e) => warn!("Error sending signed ping message: {}", e),
				}
			},
			Err(e) => {
				warn!("Error creating signed ping message: {}", e);
			},
		}
	}
}
