use color_eyre::{eyre::WrapErr, Result};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tracing::{debug, error, info};

use crate::{
	network::p2p::Client as P2pClient,
	shutdown::Controller,
	telemetry::{MetricCounter, MetricValue, Metrics},
	types::{BlockVerified, MaintenanceConfig},
};

pub async fn process_block(
	block_number: u32,
	p2p_client: &P2pClient,
	maintenance_config: MaintenanceConfig,
	metrics: &Arc<impl Metrics>,
) -> Result<()> {
	if cfg!(not(feature = "rocksdb")) && block_number % maintenance_config.pruning_interval == 0 {
		info!(block_number, "Pruning...");
		match p2p_client.prune_expired_records(Instant::now()).await {
			Ok(pruned) => info!(block_number, pruned, "Pruning finished"),
			Err(error) => error!(block_number, "Pruning failed: {error:#}"),
		}
	}

	if block_number % maintenance_config.telemetry_flush_interval == 0 {
		info!(block_number, "Flushing metrics...");
		match metrics.flush().await {
			Ok(()) => info!(block_number, "Flushing metrics finished"),
			Err(error) => error!(block_number, "Flushing metrics failed: {error:#}"),
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

	let peers_num_metric = MetricValue::DHTConnectedPeers(peers_num);
	metrics.record(peers_num_metric).await;

	metrics
		.record(MetricValue::BlockConfidenceThreshold(
			maintenance_config.block_confidence_treshold,
		))
		.await;
	metrics
		.record(MetricValue::DHTReplicationFactor(
			maintenance_config.replication_factor,
		))
		.await;
	metrics
		.record(MetricValue::DHTQueryTimeout(
			maintenance_config.query_timeout.as_secs() as u32,
		))
		.await;
	metrics.count(MetricCounter::Up).await;

	info!(block_number, map_size, "Maintenance completed");
	Ok(())
}

pub async fn run(
	p2p_client: P2pClient,
	metrics: Arc<impl Metrics>,
	mut block_receiver: broadcast::Receiver<BlockVerified>,
	static_config_params: MaintenanceConfig,
	shutdown: Controller<String>,
) {
	info!("Starting maintenance...");

	loop {
		let result = match block_receiver.recv().await {
			Ok(block) => {
				process_block(block.block_num, &p2p_client, static_config_params, &metrics).await
			},
			Err(error) => Err(error.into()),
		};

		if let Err(error) = result {
			let _ = shutdown.trigger_shutdown(format!("{error:#}"));
			break;
		}
	}
}
