use color_eyre::{eyre::WrapErr, Result};
use std::{
	sync::Arc,
	time::{Duration, Instant},
};
use tokio::sync::{broadcast, mpsc::UnboundedSender, Mutex};
use tracing::{debug, error, info};

use crate::{
	network::p2p::Client as P2pClient,
	shutdown::Controller,
	types::{BlockProcessed, MaintenanceConfig},
};

pub enum OutputEvent {
	RecordStats {
		connected_peers: usize,
		block_confidence_treshold: f64,
		replication_factor: u16,
		query_timeout: u32,
	},
	CountUps,
}

pub async fn process_block(
	block_number: u32,
	p2p_client: Arc<Mutex<Option<P2pClient>>>,
	maintenance_config: MaintenanceConfig,
	event_sender: UnboundedSender<OutputEvent>,
) -> Result<()> {
	// Check if client exists
	let p2p_client = {
		let client_guard = p2p_client.lock().await;
		if let Some(client) = client_guard.as_ref() {
			client.clone()
		} else {
			debug!(
				block_number,
				"No P2P client available, skipping p2p maintenance"
			);

			if let Err(error) = event_sender.send(OutputEvent::RecordStats {
				connected_peers: 0,
				block_confidence_treshold: maintenance_config.block_confidence_treshold,
				replication_factor: maintenance_config.replication_factor,
				query_timeout: maintenance_config.query_timeout.as_secs() as u32,
			}) {
				error!(
					%error,
					block_number,
					event_type = "OUTPUT_EVENT_SEND",
					"Failed to send stats event (no P2P client)"
				);
			}

			if let Err(error) = event_sender.send(OutputEvent::CountUps) {
				error!(
					%error,
					block_number,
					event_type = "OUTPUT_EVENT_SEND",
					"Failed to send count ups event (no P2P client)"
				);
			}

			return Ok(());
		}
	};

	if cfg!(not(feature = "rocksdb"))
		&& block_number.is_multiple_of(maintenance_config.pruning_interval)
	{
		info!(block_number, "Pruning...");
		match p2p_client.prune_expired_records(Instant::now()).await {
			Ok(pruned) => info!(block_number, pruned, "Pruning finished"),
			Err(error) => error!(block_number, "Pruning failed: {error:#}"),
		}
	}

	if let Err(error) = p2p_client
		.shrink_kademlia_map()
		.await
		.wrap_err("Unable to perform Kademlia map shrink")
	{
		error!(
			%error,
			block_number,
			event_type = "KADEMLIA_MAINTENANCE",
			operation = "shrink_map",
			"Failed to shrink Kademlia map"
		);
		return Err(error);
	}

	let map_size = match p2p_client
		.get_kademlia_map_size()
		.await
		.wrap_err("Unable to get Kademlia map size")
	{
		Ok(size) => size,
		Err(error) => {
			error!(
				%error,
				block_number,
				event_type = "KADEMLIA_MAINTENANCE",
				operation = "get_map_size",
				"Failed to get Kademlia map size"
			);
			return Err(error);
		},
	};

	let (peers_num, pub_peers_num) = p2p_client.count_dht_entries().await.map_err(|error| {
		error!(%error, block_number, event_type = "DHT_MAINTENANCE", "Failed to count DHT entries");
		error
	})?;
	info!("Number of peers in the routing table: {peers_num}. Number of peers with public IPs: {pub_peers_num}.");

	let peers = p2p_client.list_connected_peers().await.map_err(|error| {
		error!(%error, event_type = "P2P_PEER_LIST", block_number, "Failed to list connected peers");
		error
	})?;
	debug!("Connected peers: {peers:?}");

	// Reconfigure Kademlia mode if needed
	if maintenance_config.automatic_server_mode {
		if let Err(error) = p2p_client
			.reconfigure_kademlia_mode(
				maintenance_config.total_memory_gb_threshold,
				maintenance_config.num_cpus_threshold,
			)
			.await
			.wrap_err("Unable to reconfigure kademlia mode")
		{
			error!(
				%error,
				block_number,
				event_type = "KADEMLIA_MAINTENANCE",
				operation = "reconfigure_mode",
				"Failed to reconfigure Kademlia mode"
			);
			return Err(error);
		}
	}

	if let Err(error) = event_sender.send(OutputEvent::RecordStats {
		connected_peers: peers_num,
		block_confidence_treshold: maintenance_config.block_confidence_treshold,
		replication_factor: maintenance_config.replication_factor,
		query_timeout: maintenance_config.query_timeout.as_secs() as u32,
	}) {
		error!(
			%error,
			block_number,
			event_type = "OUTPUT_EVENT_SEND",
			"Failed to send stats event"
		);
	}

	if let Err(error) = event_sender.send(OutputEvent::CountUps) {
		error!(
			%error,
			block_number,
			event_type = "OUTPUT_EVENT_SEND",
			"Failed to send Count Ups event"
		);
	}

	info!(block_number, map_size, "Maintenance completed");
	Ok(())
}

pub async fn run(
	p2p_client: Arc<Mutex<Option<P2pClient>>>,
	mut block_receiver: broadcast::Receiver<BlockProcessed>,
	static_config_params: MaintenanceConfig,
	shutdown: Controller<String>,
	event_sender: UnboundedSender<OutputEvent>,
	restart: Arc<Mutex<bool>>,
	restart_delay_sec: Option<u64>,
) {
	info!("Starting maintenance...");

	let restart_delay = restart_delay_sec.map(Duration::from_secs);
	let started_at = Instant::now();

	loop {
		if block_receiver.is_closed() {
			info!("Block receiver closed, exiting maintenance loop.");
			return;
		}

		if let Some(delay) = restart_delay {
			if started_at.elapsed() >= delay {
				let mut restart = restart.lock().await;
				*restart = true;
				let message = "Avail Light Client maintenance restart...".to_string();
				info!("{message}");
				if let Err(error) = shutdown.trigger_shutdown(message) {
					error!(
						%error,
						event_type = "MAINTENANCE_SHUTDOWN",
						"Failed to trigger maintenance shutdown"
					);
				}
				return;
			}
		}

		let result = match block_receiver.recv().await {
			Ok(block) => {
				process_block(
					block.block_num(),
					p2p_client.clone(),
					static_config_params,
					event_sender.clone(),
				)
				.await
			},
			Err(error) => {
				error!(
					%error,
					event_type = "BLOCK_RECEIVE",
					"Failed to receive block for maintenance"
				);
				Err(error.into())
			},
		};

		if let Err(error) = result {
			error!(
				%error,
				event_type = "MAINTENANCE_PROCESSING",
				"Block maintenance processing failed"
			);
		}
	}
}
