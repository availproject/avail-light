use anyhow::{Context, Result};
use async_trait::async_trait;
use avail_subxt::{primitives::Header, utils::H256};
use codec::Encode;
use futures::future::join_all;
use kate_recovery::{
	data,
	matrix::{Dimensions, Partition, Position},
};
use kate_recovery::{data::Cell, matrix::RowIndex};
use mockall::automock;
use sp_core::blake2_256;
use std::{sync::Arc, time::Instant};
use tokio::sync::{broadcast, mpsc::Sender};
use tracing::{error, info};

use crate::{
	network::{
		p2p::Client as P2pClient,
		rpc::{Client as RpcClient, Event},
	},
	telemetry::{MetricValue, Metrics},
	types::FatClientConfig,
	utils::extract_kate,
};

pub async fn process_block(
	partition: &Partition,
	fat_client: &impl FatClient,
	metrics: &Arc<impl Metrics>,
	cfg: &FatClientConfig,
	header: &Header,
	received_at: Instant,
) -> Result<()> {
	let block_number = header.number;
	let header_hash: H256 = Encode::using_encoded(header, blake2_256).into();

	info!(
		{ block_number, block_delay = received_at.elapsed().as_secs()},
		"Processing finalized block",
	);

	let (rows, cols, _, _) = extract_kate(&header.extension);
	let Some(dimensions) = Dimensions::new(rows, cols) else {
		info!(
			block_number,
			"Skipping block with invalid dimensions {rows}x{cols}",
		);
		return Ok(());
	};

	let mut rpc_fetched: Vec<Cell> = vec![];
	let mut begin = Instant::now();
	let positions: Vec<Position> = dimensions
		.iter_extended_partition_positions(partition)
		.collect();
	info!(
		block_number,
		"partition_cells_requested" = positions.len(),
		"Fetching partition ({}/{}) from RPC",
		partition.number,
		partition.fraction
	);

	let rpc_cells = positions.chunks(cfg.max_cells_per_rpc).collect::<Vec<_>>();
	for batch in rpc_cells
		// TODO: Filter already fetched cells since they are verified and in DHT
		.chunks(cfg.query_proof_rpc_parallel_tasks)
		.map(|e| {
			join_all(
				e.iter()
					.map(|n| fat_client.get_kate_proof(header_hash, n))
					.collect::<Vec<_>>(),
			)
		}) {
		for partition_fetched in batch
			.await
			.into_iter()
			.enumerate()
			.map(|(i, e)| e.context(format!("Failed to fetch cells from node RPC at batch {i}")))
			.collect::<Vec<_>>()
		{
			let partition_fetched_filtered = partition_fetched?
				.into_iter()
				.filter(|cell| {
					!rpc_fetched
						.iter()
						.any(move |rpc_cell| rpc_cell.position.eq(&cell.position))
				})
				.collect::<Vec<_>>();
			rpc_fetched.extend(partition_fetched_filtered.clone());
		}

		let begin = Instant::now();

		let rpc_fetched_data_cells = rpc_fetched
			.iter()
			.filter(|cell| !cell.position.is_extended())
			.collect::<Vec<_>>();
		let rpc_fetched_data_rows = data::rows(dimensions, &rpc_fetched_data_cells);
		let rows_len = rpc_fetched_data_rows.len();

		let dht_insert_rows_success_rate = fat_client
			.insert_rows_into_dht(block_number, rpc_fetched_data_rows)
			.await;
		let success_rate: f64 = dht_insert_rows_success_rate.into();
		let time_elapsed = begin.elapsed();

		info!(
			block_number,
			"DHT PUT rows operation success rate: {dht_insert_rows_success_rate}"
		);

		metrics
			.record(MetricValue::DHTPutRowsSuccess(success_rate))
			.await?;

		info!(
			block_number,
			"partition_dht_rows_insert_time_elapsed" = ?time_elapsed,
			"{rows_len} rows inserted into DHT"
		);

		metrics
			.record(MetricValue::DHTPutRowsDuration(time_elapsed.as_secs_f64()))
			.await?;
	}

	let partition_time_elapsed = begin.elapsed();
	let rpc_fetched_len = rpc_fetched.len();
	info!(
		block_number,
		"partition_retrieve_time_elapsed" = ?partition_time_elapsed,
		"partition_cells_fetched" = rpc_fetched_len,
		"Partition cells received",
	);
	metrics
		.record(MetricValue::RPCCallDuration(
			partition_time_elapsed.as_secs_f64(),
		))
		.await?;

	begin = Instant::now();

	let dht_insert_success_rate = fat_client
		.insert_cells_into_dht(block_number, rpc_fetched)
		.await;

	info!(
		block_number,
		"DHT PUT operation success rate: {}", dht_insert_success_rate
	);

	metrics
		.record(MetricValue::DHTPutSuccess(dht_insert_success_rate as f64))
		.await?;

	let dht_put_time_elapsed = begin.elapsed();
	info!(
		block_number,
		elapsed = ?dht_put_time_elapsed,
		"{rpc_fetched_len} cells inserted into DHT",
	);

	metrics
		.record(MetricValue::DHTPutDuration(
			dht_put_time_elapsed.as_secs_f64(),
		))
		.await?;

	Ok(())
}

#[async_trait]
#[automock]
pub trait FatClient {
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> f32;
	async fn insert_rows_into_dht(&self, block: u32, rows: Vec<(RowIndex, Vec<u8>)>) -> f32;
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>>;
}

#[derive(Clone)]
struct FatClientImpl {
	p2p_client: P2pClient,
	rpc_client: RpcClient,
}

pub fn new(p2p_client: P2pClient, rpc_client: RpcClient) -> impl FatClient {
	FatClientImpl {
		p2p_client,
		rpc_client,
	}
}

#[async_trait]
impl FatClient for FatClientImpl {
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> f32 {
		self.p2p_client.insert_cells_into_dht(block, cells).await
	}
	async fn insert_rows_into_dht(&self, block: u32, rows: Vec<(RowIndex, Vec<u8>)>) -> f32 {
		self.p2p_client.insert_rows_into_dht(block, rows).await
	}
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>> {
		self.rpc_client.request_kate_proof(hash, positions).await
	}
}

pub struct Channels {
	pub rpc_event_receiver: broadcast::Receiver<Event>,
	pub error_sender: Sender<anyhow::Error>,
}

pub async fn run(
	partition: Partition,
	light_client: impl FatClient,
	cfg: FatClientConfig,
	metrics: Arc<impl Metrics>,
	mut channels: Channels,
) {
	info!("Starting light client...");

	loop {
		let (header, received_at) = match channels.rpc_event_receiver.recv().await {
			Ok(event) => match event {
				Event::HeaderUpdate {
					header,
					received_at,
				} => (header, received_at),
			},
			Err(error) => {
				error!("Cannot receive message: {error}");
				return;
			},
		};

		if let Err(error) = process_block(
			&partition,
			&light_client,
			&metrics,
			&cfg,
			&header,
			received_at,
		)
		.await
		{
			error!("Cannot process block: {error}");
		}
	}
}
