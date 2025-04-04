use std::time::Duration;

use avail_light_core::data::{Database, LatestHeaderKey, MultiAddressKey, PeerIDKey};
use avail_rust::{self, Keypair};
use chrono::Utc;
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use tokio::time;
use tracing::{info, trace, warn};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PingMessage {
	pub timestamp: i64,
	pub multiaddr: Option<String>,
	pub peer_id: Option<String>,
	pub latest_block: Option<u32>,
	pub operator_address: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SignedPingMessage {
	pub message: PingMessage,
	pub signature: Vec<u8>,
	pub public_key: String,
}

pub async fn create_and_sign_ping_message(
	keypair: Keypair,
	db: impl Database + Clone,
	operator_address: Option<String>,
) -> Result<SignedPingMessage> {
	let multiaddr = db.get(MultiAddressKey);
	let peer_id = db.get(PeerIDKey);

	let latest_block = db.get(LatestHeaderKey);
	let ping_message = PingMessage {
		timestamp: Utc::now().timestamp(),
		multiaddr,
		peer_id,
		latest_block,
		operator_address,
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

pub async fn run(
	tracking_interval: Duration,
	keypair: Keypair,
	db: impl Database + Clone,
	tracker_address: String,
	operator_address: Option<String>,
) {
	info!("Tracking service started...");
	let mut interval = time::interval(tracking_interval);
	loop {
		interval.tick().await;
		match create_and_sign_ping_message(keypair.clone(), db.clone(), operator_address.clone())
			.await
		{
			Ok(signed_ping_message) => {
				let client = reqwest::Client::new();
				match client
					.post(format!("{}/ping", tracker_address.clone()))
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
