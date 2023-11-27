//! Light client for data availability sampling and verification.
//!
//! Sampling and verification are prerequisites for application client, so [`run`] function should be on main thread and exit in case of failures.
//!
//! # Flow
//!
//! * Connect to the Avail node WebSocket stream and start listening to finalized headers
//! * Generate random cells for random data sampling (8 cells currently)
//! * Retrieve cell proofs from a) DHT and/or b) via RPC call from the node, in that order
//! * Verify proof using the received cells
//! * Calculate block confidence and store it in RocksDB
//! * Insert cells to to DHT for remote fetch
//! * Notify the consumer (app client) a new block has been verified
//!
//! # Notes
//!
//! In case delay is configured, block processing is delayed for configured time.
//! In case RPC is disabled, RPC calls will be skipped.
//! In case partition is configured, block partition is fetched and inserted into DHT.

use anyhow::{Context, Result};
use async_trait::async_trait;
use avail_subxt::{primitives::Header, utils::H256};
use codec::Encode;
use futures::future::join_all;
use kate_recovery::{
	commitments, data,
	matrix::{Dimensions, Position},
};
use kate_recovery::{data::Cell, matrix::RowIndex};
use mockall::automock;
use rocksdb::DB;
use sp_core::blake2_256;
use std::{
	sync::{Arc, Mutex},
	time::Instant,
};
use tokio::sync::{broadcast, mpsc::Sender};
use tracing::{error, info};

use crate::{
	data::{store_block_header_in_db, store_confidence_in_db},
	network::{
		self,
		p2p::Client as P2pClient,
		rpc::{self, Client as RpcClient, Event},
	},
	telemetry::{MetricCounter, MetricValue, Metrics},
	types::{self, BlockVerified, LightClientConfig, OptionBlockRange, State},
	utils::{calculate_confidence, extract_kate},
};

#[async_trait]
#[automock]
pub trait LightClient {
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> f32;
	async fn insert_rows_into_dht(&self, block: u32, rows: Vec<(RowIndex, Vec<u8>)>) -> f32;
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>>;
	async fn shrink_kademlia_map(&self) -> Result<()>;
	async fn get_multiaddress_and_ip(&self) -> Result<(String, String)>;
	async fn count_dht_entries(&self) -> Result<usize>;
	fn store_block_header_in_db(&self, header: &Header, block_number: u32) -> Result<()>;
	fn store_confidence_in_db(&self, count: u32, block_number: u32) -> Result<()>;
}

#[derive(Clone)]
struct LightClientImpl {
	db: Arc<DB>,
	p2p_client: P2pClient,
	rpc_client: RpcClient,
}

pub fn new(db: Arc<DB>, p2p_client: P2pClient, rpc_client: RpcClient) -> impl LightClient {
	LightClientImpl {
		db,
		p2p_client,
		rpc_client,
	}
}

#[async_trait]
impl LightClient for LightClientImpl {
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> f32 {
		self.p2p_client.insert_cells_into_dht(block, cells).await
	}
	async fn shrink_kademlia_map(&self) -> Result<()> {
		self.p2p_client.shrink_kademlia_map().await
	}
	async fn insert_rows_into_dht(&self, block: u32, rows: Vec<(RowIndex, Vec<u8>)>) -> f32 {
		self.p2p_client.insert_rows_into_dht(block, rows).await
	}
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>> {
		self.rpc_client.request_kate_proof(hash, positions).await
	}
	async fn get_multiaddress_and_ip(&self) -> Result<(String, String)> {
		self.p2p_client.get_multiaddress_and_ip().await
	}
	async fn count_dht_entries(&self) -> Result<usize> {
		self.p2p_client.count_dht_entries().await
	}
	fn store_confidence_in_db(&self, count: u32, block_number: u32) -> Result<()> {
		store_confidence_in_db(self.db.clone(), block_number, count)
			.context("Failed to store confidence in DB")
	}
	fn store_block_header_in_db(&self, header: &Header, block_number: u32) -> Result<()> {
		store_block_header_in_db(self.db.clone(), block_number, header)
			.context("Failed to store block header in DB")
	}
}

pub async fn process_block(
	light_client: &impl LightClient,
	network_client: &impl network::Client,
	metrics: &Arc<impl Metrics>,
	cfg: &LightClientConfig,
	header: &Header,
	received_at: Instant,
	state: Arc<Mutex<State>>,
) -> Result<Option<f64>> {
	metrics.count(MetricCounter::SessionBlock).await;
	metrics
		.record(MetricValue::TotalBlockNumber(header.number))
		.await?;

	let block_number = header.number;
	let header_hash: H256 = Encode::using_encoded(header, blake2_256).into();

	info!(
		{ block_number, block_delay = received_at.elapsed().as_secs()},
		"Processing finalized block",
	);

	let (rows, cols, _, commitment) = extract_kate(&header.extension);
	let Some(dimensions) = Dimensions::new(rows, cols) else {
		info!(
			block_number,
			"Skipping block with invalid dimensions {rows}x{cols}",
		);
		return Ok(None);
	};

	if dimensions.cols().get() <= 2 {
		error!(block_number, "more than 2 columns is required");
		return Ok(None);
	}

	let commitments = commitments::from_slice(&commitment)?;
	let cell_count = rpc::cell_count_for_confidence(cfg.confidence);
	let positions = rpc::generate_random_cells(dimensions, cell_count);
	info!(
		block_number,
		"cells_requested" = positions.len(),
		"Random cells generated: {}",
		positions.len()
	);

	let (fetched, unfetched, fetch_stats) = network_client
		.fetch_verified(
			block_number,
			header_hash,
			dimensions,
			&commitments,
			&positions,
		)
		.await?;

	metrics
		.record(MetricValue::DHTFetched(fetch_stats.dht_fetched))
		.await?;

	metrics
		.record(MetricValue::DHTFetchedPercentage(
			fetch_stats.dht_fetched_percentage,
		))
		.await?;

	metrics
		.record(MetricValue::DHTFetchDuration(
			fetch_stats.dht_fetch_duration,
		))
		.await?;

	if let Some(rpc_fetched) = fetch_stats.rpc_fetched {
		metrics
			.record(MetricValue::NodeRPCFetched(rpc_fetched))
			.await?;
	}

	if let Some(rpc_fetch_duration) = fetch_stats.rpc_fetch_duration {
		metrics
			.record(MetricValue::NodeRPCFetchDuration(rpc_fetch_duration))
			.await?;
	}

	if let Some(dht_put_success_rate) = fetch_stats.dht_put_success_rate {
		metrics
			.record(MetricValue::DHTPutSuccess(dht_put_success_rate))
			.await?;
	}

	if let Some(dht_put_duration) = fetch_stats.dht_put_duration {
		metrics
			.record(MetricValue::DHTPutDuration(dht_put_duration))
			.await?;
	}

	if positions.len() > fetched.len() {
		error!(block_number, "Failed to fetch {} cells", unfetched.len());
		return Ok(None);
	}

	// write confidence factor into on-disk database
	light_client
		.store_confidence_in_db(fetched.len() as u32, block_number)
		.context("Failed to store confidence in DB")?;

	state.lock().unwrap().confidence_achieved.set(block_number);

	let confidence = calculate_confidence(fetched.len() as u32);
	info!(
		block_number,
		"confidence" = confidence,
		"Confidence factor: {}",
		confidence
	);
	metrics
		.record(MetricValue::BlockConfidence(confidence))
		.await?;

	// push latest mined block's header into column family specified
	// for keeping block headers, to be used
	// later for verifying DHT stored data
	//
	// @note this same data store is also written to in
	// another competing thread, which syncs all block headers
	// in range [0, LATEST], where LATEST = latest block number
	// when this process started
	light_client
		.store_block_header_in_db(header, block_number)
		.context("Failed to store block header in DB")?;

	let mut rpc_fetched: Vec<Cell> = vec![];
	let mut begin = Instant::now();
	if let Some(partition) = &cfg.block_matrix_partition {
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
						.map(|n| light_client.get_kate_proof(header_hash, n))
						.collect::<Vec<_>>(),
				)
			}) {
			for partition_fetched in batch
				.await
				.into_iter()
				.enumerate()
				.map(|(i, e)| {
					e.context(format!("Failed to fetch cells from node RPC at batch {i}"))
				})
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
		}

		let begin = Instant::now();

		let rpc_fetched_data_cells = rpc_fetched
			.iter()
			.filter(|cell| !cell.position.is_extended())
			.collect::<Vec<_>>();
		let rpc_fetched_data_rows = data::rows(dimensions, &rpc_fetched_data_cells);
		let rows_len = rpc_fetched_data_rows.len();

		let dht_insert_rows_success_rate = light_client
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

	let dht_insert_success_rate = light_client
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

	light_client
		.shrink_kademlia_map()
		.await
		.context("Unable to perform Kademlia map shrink")?;

	// dump what we have on the current p2p network
	if let Ok((multiaddr, ip)) = light_client.get_multiaddress_and_ip().await {
		// set Multiaddress
		metrics.set_multiaddress(multiaddr).await;
		metrics.set_ip(ip).await;
	}
	if let Ok(counted_peers) = light_client.count_dht_entries().await {
		metrics
			.record(MetricValue::KadRoutingPeerNum(counted_peers))
			.await?
	}

	metrics.record(MetricValue::HealthCheck()).await?;

	Ok(Some(confidence))
}

pub struct Channels {
	pub block_sender: Option<broadcast::Sender<BlockVerified>>,
	pub rpc_event_receiver: broadcast::Receiver<Event>,
	pub error_sender: Sender<anyhow::Error>,
}

/// Runs light client.
///
/// # Arguments
///
/// * `light_client` - Light client implementation
/// * `cfg` - Light client configuration
/// * `block_tx` - Channel used to send header of verified block
/// * `registry` - Prometheus metrics registry
/// * `state` - Processed blocks state
pub async fn run(
	light_client: impl LightClient,
	network_client: impl network::Client,
	cfg: LightClientConfig,
	metrics: Arc<impl Metrics>,
	state: Arc<Mutex<State>>,
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

		if let Some(seconds) = cfg.block_processing_delay.sleep_duration(received_at) {
			if let Err(error) = metrics
				.record(MetricValue::BlockProcessingDelay(seconds.as_secs_f64()))
				.await
			{
				error!("Cannot record block processing delay: {}", error);
			}
			info!("Sleeping for {seconds:?} seconds");
			tokio::time::sleep(seconds).await;
		}

		let process_block_result = process_block(
			&light_client,
			&network_client,
			&metrics,
			&cfg,
			&header,
			received_at,
			state.clone(),
		)
		.await;
		let confidence = match process_block_result {
			Ok(confidence) => confidence,
			Err(error) => {
				error!("Cannot process block: {error}");
				if let Err(error) = channels.error_sender.send(error).await {
					error!("Cannot send error message: {error}");
				}
				return;
			},
		};

		let Ok(client_msg) = types::BlockVerified::try_from((header, confidence)) else {
			error!("Cannot create message from header");
			continue;
		};

		// notify dht-based application client
		// that newly mined block has been received
		if let Some(ref channel) = channels.block_sender {
			if let Err(error) = channel.send(client_msg) {
				error!("Cannot send block verified message: {error}");
				continue;
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::{
		network::rpc::{cell_count_for_confidence, CELL_COUNT_99_99},
		telemetry,
		types::RuntimeConfig,
	};
	use avail_subxt::{
		api::runtime_types::avail_core::{
			data_lookup::compact::CompactDataLookup,
			header::extension::{v1::HeaderExtension, HeaderExtension::V1},
			kate_commitment::v1::KateCommitment,
		},
		config::substrate::Digest,
	};
	use hex_literal::hex;
	use test_case::test_case;

	#[test_case(99.9 => 10)]
	#[test_case(99.99 => CELL_COUNT_99_99)]
	#[test_case(60.0 => 2)]
	#[test_case(100.0 => CELL_COUNT_99_99)]
	#[test_case(99.99999999 => CELL_COUNT_99_99)]
	#[test_case(49.0 => 8)]
	#[test_case(50.0 => 1)]
	#[test_case(50.1 => 2)]
	fn test_cell_count_for_confidence(confidence: f64) -> u32 {
		cell_count_for_confidence(confidence)
	}

	#[tokio::test]
	async fn test_process_block_with_rpc() {
		let mut mock_client = MockLightClient::new();
		let mut mock_network_client = network::MockClient::new();
		let cfg = LightClientConfig::from(&RuntimeConfig::default());
		let cells_fetched: Vec<Cell> = vec![];
		let cells_unfetched = [
			Position { row: 1, col: 3 },
			Position { row: 0, col: 0 },
			Position { row: 1, col: 2 },
			Position { row: 0, col: 1 },
		]
		.to_vec();
		let header = Header {
			parent_hash: hex!("c454470d840bc2583fcf881be4fd8a0f6daeac3a20d83b9fd4865737e56c9739")
				.into(),
			number: 57,
			state_root: hex!("7dae455e5305263f29310c60c0cc356f6f52263f9f434502121e8a40d5079c32")
				.into(),
			extrinsics_root: hex!(
				"bf1c73d4d09fa6a437a411a935ad3ec56a67a35e7b21d7676a5459b55b397ad4"
			)
			.into(),
			digest: Digest { logs: vec![] },
			extension: V1(HeaderExtension {
				commitment: KateCommitment {
					rows: 1,
					cols: 4,
					data_root: hex!(
						"0000000000000000000000000000000000000000000000000000000000000000"
					)
					.into(),
					commitment: [
						128, 34, 252, 194, 232, 229, 27, 124, 216, 33, 253, 23, 251, 126, 112, 244,
						7, 231, 73, 242, 0, 20, 5, 116, 175, 104, 27, 50, 45, 111, 127, 123, 202,
						255, 63, 192, 243, 236, 62, 75, 104, 86, 36, 198, 134, 27, 182, 224, 128,
						34, 252, 194, 232, 229, 27, 124, 216, 33, 253, 23, 251, 126, 112, 244, 7,
						231, 73, 242, 0, 20, 5, 116, 175, 104, 27, 50, 45, 111, 127, 123, 202, 255,
						63, 192, 243, 236, 62, 75, 104, 86, 36, 198, 134, 27, 182, 224,
					]
					.to_vec(),
				},
				app_lookup: CompactDataLookup {
					size: 1,
					index: vec![],
				},
			}),
		};
		let state = Arc::new(Mutex::new(State::default()));
		let recv = Instant::now();
		let kate_proof = [
			Cell {
				position: Position { row: 0, col: 2 },
				content: [
					183, 215, 10, 175, 218, 48, 236, 18, 30, 163, 215, 125, 205, 130, 176, 227,
					133, 157, 194, 35, 153, 144, 141, 7, 208, 133, 170, 79, 27, 176, 202, 22, 111,
					63, 107, 147, 93, 44, 82, 137, 78, 32, 161, 175, 214, 152, 125, 50, 247, 52,
					138, 161, 52, 83, 193, 255, 17, 235, 98, 10, 88, 241, 25, 186, 3, 174, 139,
					200, 128, 117, 255, 213, 200, 4, 46, 244, 219, 5, 131, 0,
				],
			},
			Cell {
				position: Position { row: 1, col: 1 },
				content: [
					172, 213, 85, 167, 89, 247, 11, 125, 149, 170, 217, 222, 86, 157, 11, 20, 154,
					21, 173, 247, 193, 99, 189, 7, 225, 80, 156, 94, 83, 213, 217, 185, 113, 187,
					112, 20, 170, 120, 50, 171, 52, 178, 209, 244, 158, 24, 129, 236, 83, 4, 110,
					41, 9, 29, 26, 180, 156, 219, 69, 155, 148, 49, 78, 25, 165, 147, 150, 253,
					251, 174, 49, 215, 191, 142, 169, 70, 17, 86, 218, 0,
				],
			},
			Cell {
				position: Position { row: 0, col: 3 },
				content: [
					132, 180, 92, 81, 128, 83, 245, 59, 206, 224, 200, 137, 236, 113, 109, 216,
					161, 248, 236, 252, 252, 22, 140, 107, 203, 161, 33, 18, 100, 189, 157, 58, 7,
					183, 146, 75, 57, 220, 84, 106, 203, 33, 142, 10, 130, 99, 90, 38, 85, 166,
					211, 97, 111, 105, 21, 241, 123, 211, 193, 6, 254, 125, 169, 108, 252, 85, 49,
					31, 54, 53, 79, 196, 5, 122, 206, 127, 226, 224, 70, 0,
				],
			},
			Cell {
				position: Position { row: 1, col: 3 },
				content: [
					132, 180, 92, 81, 128, 83, 245, 59, 206, 224, 200, 137, 236, 113, 109, 216,
					161, 248, 236, 252, 252, 22, 140, 107, 203, 161, 33, 18, 100, 189, 157, 58, 7,
					183, 146, 75, 57, 220, 84, 106, 203, 33, 142, 10, 130, 99, 90, 38, 85, 166,
					211, 97, 111, 105, 21, 241, 123, 211, 193, 6, 254, 125, 169, 108, 252, 85, 49,
					31, 54, 53, 79, 196, 5, 122, 206, 127, 226, 224, 70, 0,
				],
			},
		]
		.to_vec();
		mock_network_client
			.expect_fetch_verified()
			.returning(move |_, _, _, _, positions| {
				let fetched = cells_fetched.clone();
				let unfetched = cells_unfetched.clone();
				let stats = network::FetchStats::new(
					positions.len(),
					fetched.len(),
					Duration::from_secs(0),
					None,
					None,
				);
				Box::pin(async move { Ok((fetched, unfetched, stats)) })
			});
		mock_client.expect_get_kate_proof().returning(move |_, _| {
			let kate_proof = kate_proof.clone();
			Box::pin(async move { Ok(kate_proof) })
		});
		mock_client
			.expect_store_confidence_in_db()
			.returning(|_, _| Ok(()));
		mock_client
			.expect_store_block_header_in_db()
			.returning(|_, _| Ok(()));
		mock_client
			.expect_insert_rows_into_dht()
			.returning(|_, _| Box::pin(async move { 1f32 }));
		mock_client
			.expect_insert_cells_into_dht()
			.returning(|_, _| Box::pin(async move { 1f32 }));
		mock_client
			.expect_shrink_kademlia_map()
			.returning(|| Box::pin(async move { Ok(()) }));
		mock_client.expect_get_multiaddress_and_ip().returning(|| {
			Box::pin(async move { Ok(("multiaddress".to_string(), "ip".to_string())) })
		});
		mock_client
			.expect_count_dht_entries()
			.returning(|| Box::pin(async move { Ok(1) }));

		let mut mock_metrics = telemetry::MockMetrics::new();
		mock_metrics.expect_count().returning(|_| ());
		mock_metrics.expect_record().returning(|_| Ok(()));
		mock_metrics.expect_set_multiaddress().returning(|_| ());
		mock_metrics.expect_set_ip().returning(|_| ());
		process_block(
			&mock_client,
			&mock_network_client,
			&Arc::new(mock_metrics),
			&cfg,
			&header,
			recv,
			state,
		)
		.await
		.unwrap();
	}

	#[tokio::test]
	async fn test_process_block_without_rpc() {
		let mut mock_client = MockLightClient::new();
		let mut mock_network_client = network::MockClient::new();
		let mut cfg = LightClientConfig::from(&RuntimeConfig::default());
		cfg.disable_rpc = true;
		let cells_unfetched: Vec<Position> = vec![];
		let header = Header {
			parent_hash: hex!("c454470d840bc2583fcf881be4fd8a0f6daeac3a20d83b9fd4865737e56c9739")
				.into(),
			number: 57,
			state_root: hex!("7dae455e5305263f29310c60c0cc356f6f52263f9f434502121e8a40d5079c32")
				.into(),
			extrinsics_root: hex!(
				"bf1c73d4d09fa6a437a411a935ad3ec56a67a35e7b21d7676a5459b55b397ad4"
			)
			.into(),
			digest: Digest { logs: vec![] },
			extension: V1(HeaderExtension {
				commitment: KateCommitment {
					rows: 1,
					cols: 4,
					data_root: hex!(
						"0000000000000000000000000000000000000000000000000000000000000000"
					)
					.into(),
					commitment: [
						128, 34, 252, 194, 232, 229, 27, 124, 216, 33, 253, 23, 251, 126, 112, 244,
						7, 231, 73, 242, 0, 20, 5, 116, 175, 104, 27, 50, 45, 111, 127, 123, 202,
						255, 63, 192, 243, 236, 62, 75, 104, 86, 36, 198, 134, 27, 182, 224, 128,
						34, 252, 194, 232, 229, 27, 124, 216, 33, 253, 23, 251, 126, 112, 244, 7,
						231, 73, 242, 0, 20, 5, 116, 175, 104, 27, 50, 45, 111, 127, 123, 202, 255,
						63, 192, 243, 236, 62, 75, 104, 86, 36, 198, 134, 27, 182, 224,
					]
					.to_vec(),
				},
				app_lookup: CompactDataLookup {
					size: 1,
					index: vec![],
				},
			}),
		};
		let state = Arc::new(Mutex::new(State::default()));
		let recv = Instant::now();
		let cells_fetched = [
			Cell {
				position: Position { row: 0, col: 2 },
				content: [
					183, 215, 10, 175, 218, 48, 236, 18, 30, 163, 215, 125, 205, 130, 176, 227,
					133, 157, 194, 35, 153, 144, 141, 7, 208, 133, 170, 79, 27, 176, 202, 22, 111,
					63, 107, 147, 93, 44, 82, 137, 78, 32, 161, 175, 214, 152, 125, 50, 247, 52,
					138, 161, 52, 83, 193, 255, 17, 235, 98, 10, 88, 241, 25, 186, 3, 174, 139,
					200, 128, 117, 255, 213, 200, 4, 46, 244, 219, 5, 131, 0,
				],
			},
			Cell {
				position: Position { row: 1, col: 1 },
				content: [
					172, 213, 85, 167, 89, 247, 11, 125, 149, 170, 217, 222, 86, 157, 11, 20, 154,
					21, 173, 247, 193, 99, 189, 7, 225, 80, 156, 94, 83, 213, 217, 185, 113, 187,
					112, 20, 170, 120, 50, 171, 52, 178, 209, 244, 158, 24, 129, 236, 83, 4, 110,
					41, 9, 29, 26, 180, 156, 219, 69, 155, 148, 49, 78, 25, 165, 147, 150, 253,
					251, 174, 49, 215, 191, 142, 169, 70, 17, 86, 218, 0,
				],
			},
			Cell {
				position: Position { row: 0, col: 3 },
				content: [
					132, 180, 92, 81, 128, 83, 245, 59, 206, 224, 200, 137, 236, 113, 109, 216,
					161, 248, 236, 252, 252, 22, 140, 107, 203, 161, 33, 18, 100, 189, 157, 58, 7,
					183, 146, 75, 57, 220, 84, 106, 203, 33, 142, 10, 130, 99, 90, 38, 85, 166,
					211, 97, 111, 105, 21, 241, 123, 211, 193, 6, 254, 125, 169, 108, 252, 85, 49,
					31, 54, 53, 79, 196, 5, 122, 206, 127, 226, 224, 70, 0,
				],
			},
			Cell {
				position: Position { row: 1, col: 3 },
				content: [
					132, 180, 92, 81, 128, 83, 245, 59, 206, 224, 200, 137, 236, 113, 109, 216,
					161, 248, 236, 252, 252, 22, 140, 107, 203, 161, 33, 18, 100, 189, 157, 58, 7,
					183, 146, 75, 57, 220, 84, 106, 203, 33, 142, 10, 130, 99, 90, 38, 85, 166,
					211, 97, 111, 105, 21, 241, 123, 211, 193, 6, 254, 125, 169, 108, 252, 85, 49,
					31, 54, 53, 79, 196, 5, 122, 206, 127, 226, 224, 70, 0,
				],
			},
		]
		.to_vec();
		mock_network_client
			.expect_fetch_verified()
			.returning(move |_, _, _, _, positions| {
				let fetched = cells_fetched.clone();
				let unfetched = cells_unfetched.clone();
				let stats = network::FetchStats::new(
					positions.len(),
					fetched.len(),
					Duration::from_secs(0),
					None,
					None,
				);
				Box::pin(async move { Ok((fetched, unfetched, stats)) })
			});
		mock_client.expect_get_kate_proof().never();
		mock_client
			.expect_store_confidence_in_db()
			.returning(|_, _| Ok(()));
		mock_client
			.expect_store_block_header_in_db()
			.returning(|_, _| Ok(()));
		mock_client
			.expect_insert_rows_into_dht()
			.returning(|_, _| Box::pin(async move { 1f32 }));
		mock_client
			.expect_insert_cells_into_dht()
			.returning(|_, _| Box::pin(async move { 1f32 }));
		mock_client
			.expect_shrink_kademlia_map()
			.returning(|| Box::pin(async move { Ok(()) }));
		mock_client.expect_get_multiaddress_and_ip().returning(|| {
			Box::pin(async move { Ok(("multiaddress".to_string(), "ip".to_string())) })
		});
		mock_client
			.expect_count_dht_entries()
			.returning(|| Box::pin(async move { Ok(1) }));

		let mut mock_metrics = telemetry::MockMetrics::new();
		mock_metrics.expect_count().returning(|_| ());
		mock_metrics.expect_record().returning(|_| Ok(()));
		mock_metrics.expect_set_multiaddress().returning(|_| ());
		mock_metrics.expect_set_ip().returning(|_| ());
		process_block(
			&mock_client,
			&mock_network_client,
			&Arc::new(mock_metrics),
			&cfg,
			&header,
			recv,
			state,
		)
		.await
		.unwrap();
	}
}
