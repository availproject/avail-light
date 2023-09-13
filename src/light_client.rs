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
use avail_subxt::{avail, primitives::Header, utils::H256};
use codec::Encode;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
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
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{error, info};

use crate::{
	data::{store_block_header_in_db, store_confidence_in_db},
	network::Client,
	proof, rpc,
	telemetry::{MetricCounter, MetricValue, Metrics},
	types::{self, BlockVerified, LightClientConfig, State},
	utils::{calculate_confidence, extract_kate},
};

#[async_trait]
#[automock]
pub trait LightClient {
	async fn fetch_cells_from_dht(
		&self,
		positions: &[Position],
		block_number: u32,
	) -> (Vec<Cell>, Vec<Position>);
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> f32;
	async fn insert_rows_into_dht(&self, block: u32, rows: Vec<(RowIndex, Vec<u8>)>) -> f32;
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>>;
	async fn shrink_kademlia_map(&self) -> Result<()>;
	async fn network_stats(&self) -> Result<()>;
	fn store_block_header_in_db(&self, header: &Header, block_number: u32) -> Result<()>;
	fn store_confidence_in_db(&self, count: u32, block_number: u32) -> Result<()>;
}

#[derive(Clone)]
struct LightClientImpl {
	db: Arc<DB>,
	network_client: Client,
	rpc_client: avail::Client,
}

pub fn new(db: Arc<DB>, network_client: Client, rpc_client: avail::Client) -> impl LightClient {
	LightClientImpl {
		db,
		network_client,
		rpc_client,
	}
}

#[async_trait]
impl LightClient for LightClientImpl {
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> f32 {
		self.network_client
			.insert_cells_into_dht(block, cells)
			.await
	}
	async fn shrink_kademlia_map(&self) -> Result<()> {
		self.network_client.shrink_kademlia_map().await
	}
	async fn network_stats(&self) -> Result<()> {
		self.network_client.network_stats().await
	}
	async fn insert_rows_into_dht(&self, block: u32, rows: Vec<(RowIndex, Vec<u8>)>) -> f32 {
		self.network_client.insert_rows_into_dht(block, rows).await
	}
	async fn fetch_cells_from_dht(
		&self,
		positions: &[Position],
		block_number: u32,
	) -> (Vec<Cell>, Vec<Position>) {
		self.network_client
			.fetch_cells_from_dht(block_number, positions)
			.await
	}
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>> {
		rpc::get_kate_proof(&self.rpc_client, hash, positions).await
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
	metrics: &Arc<impl Metrics>,
	cfg: &LightClientConfig,
	pp: Arc<PublicParameters>,
	header: &Header,
	received_at: Instant,
	state: Arc<Mutex<State>>,
) -> Result<()> {
	metrics.count(MetricCounter::SessionBlock);
	metrics.record(MetricValue::TotalBlockNumber(header.number))?;

	let block_number = header.number;
	let header_hash: H256 = Encode::using_encoded(header, blake2_256).into();

	info!(
		{ block_number, block_delay = received_at.elapsed().as_secs()},
		"Processing finalized block",
	);

	let begin = Instant::now();

	let (rows, cols, _, commitment) = extract_kate(&header.extension);
	let Some(dimensions) = Dimensions::new(rows, cols) else {
		info!(
			block_number,
			"Skipping block with invalid dimensions {rows}x{cols}",
		);
		return Ok(());
	};

	if dimensions.cols().get() <= 2 {
		error!(block_number, "more than 2 columns is required");
		return Ok(());
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

	let (cells_fetched, unfetched) = light_client
		.fetch_cells_from_dht(&positions, block_number)
		.await;
	info!(
		block_number,
		"cells_from_dht" = cells_fetched.len(),
		"Number of cells fetched from DHT: {}",
		cells_fetched.len()
	);
	metrics.record(MetricValue::DHTFetched(cells_fetched.len() as f64))?;

	metrics.record(MetricValue::DHTFetchedPercentage(
		cells_fetched.len() as f64 / positions.len() as f64,
	))?;

	let mut rpc_fetched = if cfg.disable_rpc {
		vec![]
	} else {
		light_client
			.get_kate_proof(header_hash, &unfetched)
			.await
			.context("Failed to fetch cells from node RPC")?
	};

	info!(
		block_number,
		"cells_from_rpc" = rpc_fetched.len(),
		"Number of cells fetched from RPC: {}",
		rpc_fetched.len()
	);
	metrics.record(MetricValue::NodeRPCFetched(rpc_fetched.len() as f64))?;

	let mut cells = vec![];
	cells.extend(cells_fetched);
	cells.extend(rpc_fetched.clone());

	if positions.len() > cells.len() {
		error!(
			block_number,
			"Failed to fetch {} cells",
			positions.len() - cells.len()
		);
		return Ok(());
	}

	if !cfg.disable_proof_verification {
		let (verified, unverified) =
			proof::verify(block_number, dimensions, &cells, &commitments, pp)?;
		let count = verified.len() - unverified.len();
		info!(
			block_number,
			elapsed = ?begin.elapsed(),
			"Completed {count} verification rounds",
		);

		// write confidence factor into on-disk database
		light_client
			.store_confidence_in_db(verified.len() as u32, block_number)
			.context("Failed to store confidence in DB")?;

		state.lock().unwrap().set_confidence_achieved(block_number);

		let conf = calculate_confidence(verified.len() as u32);
		info!(
			block_number,
			"confidence" = conf,
			"Confidence factor: {}",
			conf
		);
		metrics.record(MetricValue::BlockConfidence(conf))?;
	}

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

		metrics.record(MetricValue::DHTPutRowsSuccess(success_rate))?;

		info!(
			block_number,
			"partition_dht_rows_insert_time_elapsed" = ?time_elapsed,
			"{rows_len} rows inserted into DHT"
		);

		metrics.record(MetricValue::DHTPutRowsDuration(time_elapsed.as_secs_f64()))?;
	}

	let partition_time_elapsed = begin.elapsed();
	let rpc_fetched_len = rpc_fetched.len();
	info!(
		block_number,
		"partition_retrieve_time_elapsed" = ?partition_time_elapsed,
		"partition_cells_fetched" = rpc_fetched_len,
		"Partition cells received",
	);
	metrics.record(MetricValue::RPCCallDuration(
		partition_time_elapsed.as_secs_f64(),
	))?;

	begin = Instant::now();

	let dht_insert_success_rate = light_client
		.insert_cells_into_dht(block_number, rpc_fetched)
		.await;

	info!(
		block_number,
		"DHT PUT operation success rate: {}", dht_insert_success_rate
	);

	metrics.record(MetricValue::DHTPutSuccess(dht_insert_success_rate as f64))?;

	let dht_put_time_elapsed = begin.elapsed();
	info!(
		block_number,
		elapsed = ?dht_put_time_elapsed,
		"{rpc_fetched_len} cells inserted into DHT",
	);

	metrics.record(MetricValue::DHTPutDuration(
		dht_put_time_elapsed.as_secs_f64(),
	))?;

	light_client
		.shrink_kademlia_map()
		.await
		.context("Unable to perform Kademlia map shrink")?;

	light_client
		.network_stats()
		.await
		.context("Unable to dump network stats")?;

	Ok(())
}

pub struct Channels {
	pub block_sender: Option<Sender<BlockVerified>>,
	pub header_receiver: Receiver<(Header, Instant)>,
	pub error_sender: Sender<anyhow::Error>,
}

/// Runs light client.
///
/// # Arguments
///
/// * `light_client` - Light client implementation
/// * `cfg` - Light client configuration
/// * `block_tx` - Channel used to send header of verified block
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
/// * `registry` - Prometheus metrics registry
/// * `state` - Processed blocks state
pub async fn run(
	light_client: impl LightClient,
	cfg: LightClientConfig,
	pp: Arc<PublicParameters>,
	metrics: Arc<impl Metrics>,
	state: Arc<Mutex<State>>,
	mut channels: Channels,
) {
	info!("Starting light client...");

	while let Some((header, received_at)) = channels.header_receiver.recv().await {
		if let Some(seconds) = cfg.block_processing_delay.sleep_duration(received_at) {
			info!("Sleeping for {seconds:?} seconds");
			tokio::time::sleep(seconds).await;
		}

		if let Err(error) = process_block(
			&light_client,
			&metrics,
			&cfg,
			pp.clone(),
			&header,
			received_at,
			state.clone(),
		)
		.await
		{
			error!("Cannot process block: {error}");
			if let Err(error) = channels.error_sender.send(error).await {
				error!("Cannot send error message: {error}");
			}
			return;
		}

		let Ok(client_msg) = types::BlockVerified::try_from(header) else {
			error!("Cannot create message from header");
			continue;
		};

		// notify dht-based application client
		// that newly mined block has been received
		if let Some(ref channel) = channels.block_sender {
			if let Err(error) = channel.send(client_msg).await {
				error!("Cannot send block verified message: {error}");
				continue;
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::rpc::cell_count_for_confidence;
	use super::*;
	use crate::telemetry;
	use crate::types::RuntimeConfig;
	use avail_subxt::{
		api::runtime_types::avail_core::{
			data_lookup::compact::CompactDataLookup,
			header::extension::{v1::HeaderExtension, HeaderExtension::V1},
			kate_commitment::v1::KateCommitment,
		},
		config::substrate::Digest,
	};
	use hex_literal::hex;
	use kate_recovery::testnet;

	#[test]
	fn test_cell_count_for_confidence() {
		let count = 1;
		assert!(cell_count_for_confidence(60f64) > count);
		assert_eq!(
			cell_count_for_confidence(100f64),
			(-((1f64 - (99f64 / 100f64)).log2())).ceil() as u32
		);
		assert_eq!(
			cell_count_for_confidence(49f64),
			(-((1f64 - (99f64 / 100f64)).log2())).ceil() as u32
		);
		assert!(
			(cell_count_for_confidence(99.99999999)) < 10
				&& (cell_count_for_confidence(99.99999999)) > 0
		);
	}

	#[tokio::test]
	async fn test_process_block_with_rpc() {
		let mut mock_client = MockLightClient::new();
		let cfg = LightClientConfig::from(&RuntimeConfig::default());
		let pp = Arc::new(testnet::public_params(1024));
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
		mock_client
			.expect_fetch_cells_from_dht()
			.returning(move |_, _| {
				let fetched = cells_fetched.clone();
				let unfetched = cells_unfetched.clone();
				Box::pin(async move { (fetched, unfetched) })
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
		mock_client
			.expect_network_stats()
			.returning(|| Box::pin(async move { Ok(()) }));
		let mut mock_metrics = telemetry::MockMetrics::new();
		mock_metrics.expect_count().returning(|_| ());
		mock_metrics.expect_record().returning(|_| Ok(()));
		process_block(
			&mock_client,
			&Arc::new(mock_metrics),
			&cfg,
			pp,
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
		let mut cfg = LightClientConfig::from(&RuntimeConfig::default());
		cfg.disable_rpc = true;
		let pp = Arc::new(testnet::public_params(1024));
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
		mock_client
			.expect_fetch_cells_from_dht()
			.returning(move |_, _| {
				let fetched = cells_fetched.clone();
				let unfetched = cells_unfetched.clone();
				Box::pin(async move { (fetched, unfetched) })
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
		mock_client
			.expect_network_stats()
			.returning(|| Box::pin(async move { Ok(()) }));
		let mut mock_metrics = telemetry::MockMetrics::new();
		mock_metrics.expect_count().returning(|_| ());
		mock_metrics.expect_record().returning(|_| Ok(()));
		process_block(
			&mock_client,
			&Arc::new(mock_metrics),
			&cfg,
			pp,
			&header,
			recv,
			state,
		)
		.await
		.unwrap();
	}
}
