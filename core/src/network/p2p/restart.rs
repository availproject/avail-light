use color_eyre::{eyre::WrapErr, Result};
use libp2p::{identity::Keypair, PeerId};
use rand::Rng;
use std::{sync::Arc, time::Duration};
use tokio::{
	select,
	sync::mpsc::{UnboundedReceiver, UnboundedSender},
	sync::Mutex,
};
use tracing::{error, info, warn};

use crate::{
	data::{Database, PeerIDKey, DB},
	network::p2p::{self, Client, OutputEvent},
	shutdown::Controller,
	types::ProjectName,
	utils::{self, spawn_in_span},
};

use super::configuration::LibP2PConfig;

/// Initialize and start P2P client with all necessary components
#[allow(clippy::too_many_arguments)]
pub async fn init_and_start_p2p_client(
	libp2p_cfg: &LibP2PConfig,
	project_name: ProjectName,
	genesis_hash: &str,
	id_keys: Keypair,
	peer_id: PeerId,
	version: &str,
	shutdown: Controller<String>,
	db: DB,
) -> Result<(Client, UnboundedReceiver<OutputEvent>)> {
	let (mut p2p_client, p2p_event_loop, p2p_event_receiver) = p2p::init(
		libp2p_cfg.clone(),
		project_name,
		id_keys,
		version,
		genesis_hash,
		false,
		shutdown.clone(),
		#[cfg(feature = "rocksdb")]
		db.clone(),
	)
	.await?;

	// Start the new event loop
	spawn_in_span(shutdown.with_cancel(p2p_event_loop.run()));

	let addrs = vec![libp2p_cfg.tcp_multiaddress()];

	// Start the TCP and WebRTC listeners
	p2p_client
		.start_listening(addrs)
		.await
		.wrap_err("Error starting listener.")?;
	info!("TCP listener started on port {}", libp2p_cfg.port);

	db.put(PeerIDKey, peer_id.to_string());

	let p2p_clone = p2p_client.to_owned();
	let bootstraps = libp2p_cfg.bootstraps.clone();
	spawn_in_span(shutdown.with_cancel(async move {
		info!("Bootstrapping the DHT with bootstrap nodes...");
		if let Err(error) = p2p_clone.bootstrap_on_startup(&bootstraps).await {
			info!("Bootstrap unsuccessful: {error:#}");
		}
	}));

	Ok((p2p_client, p2p_event_receiver))
}

/// P2P restart manager that periodically restarts the P2P event loop
#[allow(clippy::too_many_arguments)]
pub async fn p2p_restart_manager(
	p2p_client: Arc<Mutex<Option<Client>>>,
	libp2p_cfg: LibP2PConfig,
	project_name: ProjectName,
	genesis_hash: String,
	id_keys: Keypair,
	peer_id: PeerId,
	version: String,
	restart_interval: Duration,
	app_shutdown: Controller<String>,
	db: DB,
	event_tx: UnboundedSender<OutputEvent>,
	mut current_shutdown: Controller<String>,
) {
	// Randomize restart intervals
	let randomized_duration = randomize_duration(restart_interval);

	let mut interval = tokio::time::interval(randomized_duration);
	interval.tick().await;

	loop {
		select! {
			_ = interval.tick() => {
				info!("Starting P2P client restart process...");

				{
					let mut client_guard = p2p_client.lock().await;
					if let Some(mut client) = client_guard.take() {
						info!("Stopping listener");
						if let Err(e) = client.stop_listening().await {
							warn!("Error stopping listeners during restart: {e}");
						}
					}
				}

				// Trigger shutdown of the current event loop
				let _ = current_shutdown.trigger_shutdown("P2P client periodic restart".to_string());

				_ = current_shutdown.completed_shutdown().await;

				// Create a new shutdown controller for the next restart cycle
				let new_shutdown = Controller::<String>::new();

				// Restart the P2P client
				match init_and_start_p2p_client(
					&libp2p_cfg,
					project_name.clone(),
					&genesis_hash,
					id_keys.clone(),
					peer_id,
					&version,
					new_shutdown.clone(),
					db.clone(),
				).await {
					Ok((new_p2p_client, receiver)) => {
						info!("P2P client restarted successfully");

						// Update the shared client reference
						*p2p_client.lock().await = Some(new_p2p_client);
						// Forward events from new receiver to the main event channel
						let event_tx_clone = event_tx.clone();
						let app_shutdown_clone = app_shutdown.clone();
						spawn_in_span(app_shutdown_clone.with_cancel(async move {
							forward_p2p_events(receiver, event_tx_clone).await;
						}));

						// Update current shutdown for next restart
						current_shutdown = new_shutdown;
					}
					Err(error) => {
						error!("Failed to restart P2P client: {error:#}");
						// Continue with the next restart attempt
					}
				}
			}
			_ = app_shutdown.triggered_shutdown() => {
				info!("P2P restart manager shutting down");
				break;
			}
		}
	}
}

/// Forward P2P events from receiver to sender
pub async fn forward_p2p_events(
	mut receiver: UnboundedReceiver<OutputEvent>,
	sender: UnboundedSender<OutputEvent>,
) {
	while let Some(event) = receiver.recv().await {
		if sender.send(event).is_err() {
			break;
		}
	}
}

pub fn randomize_duration(max: Duration) -> Duration {
	let mut rng = utils::rng();
	Duration::from_secs(rng.gen_range(0..max.as_secs()))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_randomize_duration_happy_path() {
		let max_duration = Duration::from_secs(100);

		for _ in 0..100 {
			let result = randomize_duration(max_duration);

			assert!(result >= Duration::from_secs(0));
			assert!(result < max_duration);
		}
	}

	#[test]
	fn test_randomize_duration_one_second() {
		let max_duration = Duration::from_secs(1);
		let result = randomize_duration(max_duration);

		// Result should be 0 (the only valid value in range 0..1)
		assert_eq!(result, Duration::from_secs(0));
	}
}
