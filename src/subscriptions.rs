use std::{sync::mpsc::SyncSender, time::Instant};

use anyhow::{anyhow, Context};
use avail_subxt::{primitives::Header, AvailConfig};
use subxt::OnlineClient;
use tracing::{error, info};

pub async fn finalized_headers(
	rpc_client: OnlineClient<AvailConfig>,
	message_tx: SyncSender<(Header, Instant)>,
	error_sender: SyncSender<anyhow::Error>,
) {
	match rpc_client.rpc().subscribe_finalized_blocks().await {
		Ok(mut new_heads_sub) => {
			while let Some(message) = new_heads_sub.next().await {
				let received_at = Instant::now();
				if let Err(error) = message
					.context("Failed to read web socket message")
					.and_then(|header| {
						info!(header.number, "Received finalized block header");
						let message = (header, received_at);
						message_tx.send(message).context("Send failed")
					}) {
					error!("Fail to process finalized block header: {error}");
				}
			}
			error!("Finalized blocks subscription disconnected");
			let error = anyhow!("Finalized blocks subscription disconnected");
			if let Err(error) = error_sender.send(error) {
				error!("Cannot send error to error channel: {error}");
			}
		},
		Err(error) => {
			error!("Failed to subscribe to finalized blocks: {error}");
			if let Err(error) = error_sender.send(error.into()) {
				error!("Cannot send error to error channel: {error}");
			}
		},
	}
}
