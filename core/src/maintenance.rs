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
	types::{BlockVerified, MaintenanceConfig},
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
	p2p_client: Option<P2pClient>,
	maintenance_config: MaintenanceConfig,
	event_sender: UnboundedSender<OutputEvent>,
) -> Result<()> {
	// Early return if no p2p_client
	let Some(p2p_client) = p2p_client else {
		debug!(
			block_number,
			"No P2P client available, skipping p2p maintenance"
		);
		event_sender.send(OutputEvent::RecordStats {
			connected_peers: 0,
			block_confidence_treshold: maintenance_config.block_confidence_treshold,
			replication_factor: maintenance_config.replication_factor,
			query_timeout: maintenance_config.query_timeout.as_secs() as u32,
		})?;
		event_sender.send(OutputEvent::CountUps)?;
		return Ok(());
	};

	if cfg!(not(feature = "rocksdb")) && block_number % maintenance_config.pruning_interval == 0 {
		info!(block_number, "Pruning...");
		match p2p_client.prune_expired_records(Instant::now()).await {
			Ok(pruned) => info!(block_number, pruned, "Pruning finished"),
			Err(error) => error!(block_number, "Pruning failed: {error:#}"),
		}
	}

	p2p_client
		.shrink_kademlia_map()
		.await
		.wrap_err("Unable to perform Kademlia map shrink")?;

	let map_size = p2p_client
		.get_kademlia_map_size()
		.await
		.wrap_err("Unable to get Kademlia map size")?;

	let (peers_num, pub_peers_num) = p2p_client.count_dht_entries().await?;
	info!("Number of peers in the routing table: {peers_num}. Number of peers with public IPs: {pub_peers_num}.");

	let connected_peers = p2p_client.list_connected_peers().await?;
	debug!("Connected peers: {:?}", connected_peers);

	// Reconfigure Kademlia mode if needed
	if maintenance_config.automatic_server_mode {
		p2p_client
			.reconfigure_kademlia_mode(
				maintenance_config.total_memory_gb_threshold,
				maintenance_config.num_cpus_threshold,
			)
			.await
			.wrap_err("Unable to reconfigure kademlia mode")?;
	}

	event_sender.send(OutputEvent::RecordStats {
		connected_peers: peers_num,
		block_confidence_treshold: maintenance_config.block_confidence_treshold,
		replication_factor: maintenance_config.replication_factor,
		query_timeout: maintenance_config.query_timeout.as_secs() as u32,
	})?;

	event_sender.send(OutputEvent::CountUps)?;

	info!(block_number, map_size, "Maintenance completed");
	Ok(())
}

pub async fn run(
	p2p_client: Option<P2pClient>,
	mut block_receiver: broadcast::Receiver<BlockVerified>,
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
		if let Some(delay) = restart_delay {
			if started_at.elapsed() >= delay {
				let mut restart = restart.lock().await;
				*restart = true;
				let message = "Avail Light Client maintenance restart...".to_string();
				info!("{message}");
				if let Err(error) = shutdown.trigger_shutdown(message) {
					error!("{error:#}");
				}
				return;
			}
		}

		let result = match block_receiver.recv().await {
			Ok(block) => {
				process_block(
					block.block_num,
					p2p_client.clone(),
					static_config_params,
					event_sender.clone(),
				)
				.await
			},
			Err(error) => Err(error.into()),
		};

		if let Err(error) = result {
			let _ = shutdown.trigger_shutdown(format!("{error:#}"));
			break;
		}
	}
}
