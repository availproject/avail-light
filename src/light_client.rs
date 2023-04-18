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

use std::{
	sync::{Arc, Mutex},
	time::{Instant, SystemTime},
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use avail_subxt::{
	api::runtime_types::da_primitives::header::extension::HeaderExtension, primitives::Header,
	AvailConfig,
};
use codec::Encode;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use futures::{future::join_all, lock};
use kate_recovery::data::Cell;
use kate_recovery::{
	commitments, data,
	matrix::{Dimensions, Position},
};
use mockall::automock;
use rocksdb::DB;
use sp_core::blake2_256;
use subxt::{utils::H256, OnlineClient};
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{error, info};

use crate::{
	data::{store_block_header_in_db, store_confidence_in_db},
	http::calculate_confidence,
	network::Client,
	proof, rpc,
	telemetry::metrics::{MetricEvent, Metrics},
	types::{self, BlockVerified, LightClientConfig},
};

#[async_trait]
#[automock]
pub trait LightClient {
	async fn fetch_cells_from_dht(
		&self,
		positions: &[Position],
		block_number: u32,
	) -> Result<(Vec<Cell>, Vec<Position>)>;
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> Result<f32>;
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>>;
	fn verify_cells(
		&self,
		block_num: u32,
		dimensions: &Dimensions,
		cells: &[Cell],
		commitments: &[[u8; 48]],
		public_parameters: &PublicParameters,
	) -> Result<(Vec<Position>, Vec<Position>)>;
	fn store_block_header_in_db(&self, header: &Header, block_number: u32) -> Result<()>;
	fn store_confidence_in_db(&self, count: u32, block_number: u32) -> Result<bool>;
	async fn process_block(
		&self,
		cfg: &LightClientConfig,
		pp: PublicParameters,
		header: &Header,
		received_at: Instant,
		metrics: &Metrics,
		counter: Arc<Mutex<u32>>,
	) -> Result<()>;
}

#[derive(Clone)]
pub struct LightClientImpl {
	db: Arc<DB>,
	network_client: Client,
	rpc_client: OnlineClient<AvailConfig>,
}

#[async_trait]
impl LightClient for LightClientImpl {
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> Result<f32> {
		Ok(self
			.network_client
			.insert_cells_into_dht(block, cells)
			.await)
	}
	async fn fetch_cells_from_dht(
		&self,
		positions: &[Position],
		block_number: u32,
	) -> Result<(Vec<Cell>, Vec<Position>)> {
		Ok(self
			.network_client
			.fetch_cells_from_dht(block_number, positions)
			.await)
	}
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>> {
		Ok(rpc::get_kate_proof(&self.rpc_client, hash, positions).await?)
	}
	fn verify_cells(
		&self,
		block_num: u32,
		dimensions: &Dimensions,
		cells: &[Cell],
		commitments: &[[u8; 48]],
		public_parameters: &PublicParameters,
	) -> Result<(Vec<Position>, Vec<Position>)> {
		let proof = proof::verify(block_num, dimensions, cells, commitments, public_parameters)?;
		Ok(proof)
	}
	fn store_confidence_in_db(&self, count: u32, block_number: u32) -> Result<bool> {
		let x = store_confidence_in_db(self.db.clone(), block_number, count)
			.context("Failed to store confidence in DB")?;
		Ok(x)
	}
	fn store_block_header_in_db(&self, header: &Header, block_number: u32) -> Result<()> {
		store_block_header_in_db(self.db.clone(), block_number, header)
			.context("Failed to store block header in DB")?;
		Ok(())
	}
	async fn process_block(
		&self,
		cfg: &LightClientConfig,
		pp: PublicParameters,
		header: &Header,
		received_at: Instant,
		metrics: &Metrics,
		counter: Arc<Mutex<u32>>,
	) -> Result<()> {
		process_block(&self, cfg, &pp, header, received_at, metrics, counter).await?;
		Ok(())
	}
}

pub async fn process_block(
	light_client: &LightClientImpl,
	cfg: &LightClientConfig,
	// db: Arc<DB>,
	// network_client: &Client,
	// rpc_client: &OnlineClient<AvailConfig>,
	pp: &PublicParameters,
	header: &Header,
	received_at: Instant,
	metrics: &Metrics,
	counter: Arc<Mutex<u32>>,
) -> Result<()> {
	metrics.record(MetricEvent::SessionBlockCounter);
	metrics.record(MetricEvent::TotalBlockNumber(header.number));

	let block_number = header.number;
	let header_hash: H256 = Encode::using_encoded(header, blake2_256).into();

	info!(
		{ block_number, block_delay = received_at.elapsed().as_secs()},
		"Processing finalized block",
	);

	let begin = SystemTime::now();

	let HeaderExtension::V1(xt) = &header.extension;
	let Some(dimensions) = Dimensions::new(xt.commitment.rows, xt.commitment.cols) else {
			    info!(
				    block_number,
				    "Skipping block with invalid dimensions {}x{}",
				    xt.commitment.rows,
				    xt.commitment.cols
			    );
	    return Ok(());
	};

	if dimensions.cols() <= 2 {
		error!(block_number, "more than 2 columns is required");
		return Ok(());
	}

	let commitments = commitments::from_slice(&xt.commitment.commitment)?;

	let cell_count = rpc::cell_count_for_confidence(cfg.confidence);
	let positions = rpc::generate_random_cells(&dimensions, cell_count);
	info!(
		block_number,
		"cells_requested" = positions.len(),
		"Random cells generated: {}",
		positions.len()
	);

	let (cells_fetched, unfetched) = light_client
		.network_client
		.fetch_cells_from_dht(block_number, &positions)
		.await;

	info!(
		block_number,
		"cells_from_dht" = cells_fetched.len(),
		"Number of cells fetched from DHT: {}",
		cells_fetched.len()
	);
	metrics.record(MetricEvent::DHTFetched(cells_fetched.len() as i64));
	metrics.record(MetricEvent::DHTFetchedPercentage(
		cells_fetched.len() as f64 / positions.len() as f64,
	));

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
	metrics.record(MetricEvent::NodeRPCFetched(rpc_fetched.len() as i64));

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
			proof::verify(block_number, &dimensions, &cells, &commitments, pp)?;
		let count = verified.len() - unverified.len();
		info!(
			block_number,
			"Completed {count} verification rounds in \t{:?}",
			begin.elapsed()?
		);

		// write confidence factor into on-disk database
		let x = light_client
			.store_confidence_in_db(verified.len() as u32, block_number)
			.context("Failed to store confidence in DB")?;

		if x {
			info!("stored in db");
		}

		// let y = is_confidence_in_db(db.clone(),block_number)

		let mut lock = counter.lock().unwrap();
		*lock = block_number;
		info!("counter lock {:?}", lock);

		let conf = calculate_confidence(verified.len() as u32);
		info!(
			block_number,
			"confidence" = conf,
			"Confidence factor: {}",
			conf
		);
		metrics.record(MetricEvent::BlockConfidence(conf));
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

	let mut begin = SystemTime::now();
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

		let begin = SystemTime::now();

		let rpc_fetched_data_cells = rpc_fetched
			.iter()
			.filter(|cell| !cell.position.is_extended())
			.collect::<Vec<_>>();
		let rpc_fetched_data_rows = data::rows(&dimensions, &rpc_fetched_data_cells);
		let rows_len = rpc_fetched_data_rows.len();

		let dht_insert_rows_success_rate = light_client
			.network_client
			.insert_rows_into_dht(block_number, rpc_fetched_data_rows)
			.await;
		let success_rate: f64 = dht_insert_rows_success_rate.into();
		let time_elapsed = begin.elapsed()?.as_secs_f64();

		info!(
			block_number,
			"DHT PUT rows operation success rate: {dht_insert_rows_success_rate}"
		);

		metrics.record(MetricEvent::DHTPutRowsSuccess(success_rate));

		info!(
			block_number,
			"partition_dht_rows_insert_time_elapsed" = time_elapsed,
			"{rows_len} rows inserted into DHT"
		);

		metrics.record(MetricEvent::DHTPutRowsDuration(time_elapsed));
	}

	let partition_time_elapsed = begin.elapsed()?;
	let rpc_fetched_len = rpc_fetched.len();
	info!(
		block_number,
		"partition_retrieve_time_elapsed" = partition_time_elapsed.as_secs_f64(),
		"partition_cells_fetched" = rpc_fetched_len,
		"Partition cells received. Time elapsed: \t{:?}",
		partition_time_elapsed
	);
	metrics.record(MetricEvent::RPCCallDuration(
		partition_time_elapsed.as_secs_f64(),
	));

	begin = SystemTime::now();

	let dht_insert_success_rate = light_client
		.network_client
		.insert_cells_into_dht(block_number, rpc_fetched)
		.await;

	info!(
		block_number,
		"DHT PUT operation success rate: {}", dht_insert_success_rate
	);

	metrics.record(MetricEvent::DHTPutSuccess(dht_insert_success_rate as f64));

	let dht_put_time_elapsed = begin.elapsed()?;
	info!(
		block_number,
		"partition_dht_insert_time_elapsed" = dht_put_time_elapsed.as_secs_f64(),
		"{rpc_fetched_len} cells inserted into DHT. Time elapsed: \t{:?}",
		dht_put_time_elapsed
	);

	metrics.record(MetricEvent::DHTPutDuration(
		dht_put_time_elapsed.as_secs_f64(),
	));

	Ok(())
}

async fn light_process_block<T>(
	light_client: T,
	cfg: &LightClientConfig,
	pp: &PublicParameters,
	header: &Header,
	received_at: Instant,
	metrics: &Metrics,
	counter: Arc<Mutex<u32>>,
) -> Result<()>
where
	T: LightClient,
{
	Ok(light_client
		.process_block(&cfg, pp.clone(), header, received_at, metrics, counter)
		.await?)
}
/// Runs light client.
///
/// # Arguments
///
/// * `cfg` - Light client configuration
/// * `db` - Database to store confidence and block header
/// * `network_client` - Reference to a libp2p custom network client
/// * `rpc_client` - Node's RPC subxt client for fetching data unavailable in DHT (if configured)
/// * `block_tx` - Channel used to send header of verified block
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
/// * `registry` - Prometheus metrics registry
/// * `counter` - Processed block mutex counter
pub async fn run(
	cfg: LightClientConfig,
	db: Arc<DB>,
	network_client: Client,
	rpc_client: OnlineClient<AvailConfig>,
	block_tx: Option<Sender<BlockVerified>>,
	pp: PublicParameters,
	metrics: Metrics,
	counter: Arc<Mutex<u32>>,
	mut message_rx: Receiver<(Header, Instant)>,
	error_sender: Sender<anyhow::Error>,
) {
	info!("Starting light client...");

	while let Some((header, received_at)) = message_rx.recv().await {
		if let Some(seconds) = cfg.block_processing_delay.sleep_duration(received_at) {
			info!("Sleeping for {seconds:?} seconds");
			tokio::time::sleep(seconds).await;
		}
		let db_clone = db.clone();
		let light_client = LightClientImpl {
			db: db_clone,
			network_client: network_client.clone(),
			rpc_client: rpc_client.clone(),
		};

		if let Err(error) = light_process_block(
			light_client,
			&cfg,
			&pp,
			&header,
			received_at,
			&metrics,
			counter.clone(),
		)
		.await
		{
			error!("Cannot process block: {error}");
			if let Err(error) = error_sender.send(error).await {
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
		if let Some(ref channel) = block_tx {
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
	use avail_subxt::api::runtime_types::da_primitives::asdr::data_lookup::DataLookup;
	use avail_subxt::api::runtime_types::da_primitives::header::extension::v1::HeaderExtension;
	use avail_subxt::api::runtime_types::da_primitives::header::extension::HeaderExtension::V1;
	use avail_subxt::api::runtime_types::da_primitives::kate_commitment::KateCommitment;
	use hex_literal::hex;
	use kate_recovery::testnet;
	use subxt::config::substrate::Digest;
	use subxt::config::substrate::DigestItem::{PreRuntime, Seal};

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
	async fn test_rpc_and_insert_dht() {
		let mut mock_client = MockLightClient::new();
		let pos = [Position { row: 0, col: 3 }];
		let unfetched: Vec<Cell> = vec![Cell {
			position: Position { row: 0, col: 3 },
			content: [
				150, 6, 83, 12, 56, 17, 0, 225, 186, 238, 151, 181, 116, 1, 34, 240, 174, 192, 98,
				201, 60, 208, 50, 215, 90, 231, 2, 27, 17, 204, 140, 30, 213, 253, 200, 176, 72,
				98, 121, 25, 239, 76, 230, 154, 121, 246, 142, 37, 85, 184, 201, 218, 107, 88, 0,
				87, 199, 169, 98, 172, 4, 140, 151, 65, 162, 162, 190, 205, 20, 95, 67, 114, 73,
				59, 170, 52, 243, 140, 237, 0,
			],
		}];
		let unfetched_clone = unfetched.clone();
		let header_hash: H256 =
			hex!("3767f8955d6f7306b1e55701b6316fa1163daa8d4cffdb05c3b25db5f5da1723").into();
		mock_client
			.expect_get_kate_proof()
			// .with(|_, h:H256, _| h == header_hash)
			.returning(move |_, _| {
				let unfetched = unfetched.clone();
				Box::pin(async move { Ok(unfetched) })
			});
		mock_client.get_kate_proof(header_hash, &pos).await.unwrap();
		mock_client
			.expect_insert_cells_into_dht()
			.withf(move |x, _| *x == 2)
			.returning(move |_, _| Box::pin(async move { Ok(1f32) }));
		mock_client
			.insert_cells_into_dht(2, unfetched_clone)
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn test_verify_cells() {
		let pp = testnet::public_params(1024);
		// mock_expect_get_kate_pr = testnet::public_params(1024);
		let mut mock_client = MockLightClient::new();
		let header: Header = Header {
			parent_hash: hex!("2a75ea712b4b2c360cb7c0cdd806de4e9363ff7e37ce30788d487a258604dba3")
				.into(),
			number: 2,
			state_root: hex!("6f41d5a26a34f7bc3a09d4811b444c09daaebbd5c5d67c4525f42b3ed11bef86")
				.into(),
			extrinsics_root: hex!(
				"3027e34c2c75756c22770e6a3650ad68f3c9e44eed3c5ab4471742fe96678dae"
			)
			.into(),
			digest: Digest {
				logs: vec![
					PreRuntime(
						[66, 65, 66, 69],
						[2, 0, 0, 0, 0, 145, 68, 2, 5, 0, 0, 0, 0].into(),
					),
					Seal(
						[66, 65, 66, 69],
						vec![
							124, 169, 85, 4, 144, 53, 228, 107, 198, 30, 152, 128, 74, 145, 40,
							144, 122, 89, 15, 55, 192, 162, 152, 195, 109, 123, 87, 121, 142, 140,
							178, 53, 131, 106, 180, 233, 114, 82, 102, 51, 132, 176, 115, 150, 114,
							216, 116, 130, 163, 224, 150, 76, 98, 209, 14, 60, 34, 192, 95, 162,
							86, 140, 246, 143,
						],
					),
				],
			},
			extension: V1(HeaderExtension {
				commitment: KateCommitment {
					rows: 1,
					cols: 4,
					data_root: hex!(
						"0000000000000000000000000000000000000000000000000000000000000000"
					)
					.into(),
					commitment: vec![
						181, 10, 104, 251, 33, 171, 87, 192, 13, 195, 93, 127, 215, 78, 114, 192,
						95, 92, 167, 10, 49, 17, 20, 204, 222, 102, 70, 218, 173, 18, 30, 49, 232,
						10, 137, 187, 186, 216, 97, 140, 16, 33, 52, 56, 170, 208, 118, 242, 181,
						10, 104, 251, 33, 171, 87, 192, 13, 195, 93, 127, 215, 78, 114, 192, 95,
						92, 167, 10, 49, 17, 20, 204, 222, 102, 70, 218, 173, 18, 30, 49, 232, 10,
						137, 187, 186, 216, 97, 140, 16, 33, 52, 56, 170, 208, 118, 242,
					],
				},
				app_lookup: DataLookup {
					size: 1,
					index: vec![],
				},
			}),
		};

		let V1(xt) = &header.extension;
		let dimensions = Dimensions::new(xt.commitment.rows, xt.commitment.cols).unwrap();
		let commitments = commitments::from_slice(&xt.commitment.commitment).unwrap();
		let cells = vec![
			Cell {
				position: Position { row: 1, col: 1 },
				content: [
					165, 187, 167, 30, 116, 213, 60, 35, 8, 53, 187, 175, 212, 5, 173, 37, 229,
					147, 100, 43, 92, 133, 70, 203, 222, 218, 230, 148, 82, 175, 26, 252, 195, 81,
					70, 186, 215, 106, 224, 70, 86, 48, 206, 206, 246, 82, 189, 226, 83, 4, 110,
					41, 9, 29, 26, 180, 156, 219, 69, 155, 148, 49, 78, 25, 165, 147, 150, 253,
					251, 174, 49, 215, 191, 142, 169, 70, 17, 86, 218, 0,
				],
			},
			Cell {
				position: Position { row: 0, col: 3 },
				content: [
					135, 95, 122, 149, 35, 94, 140, 33, 42, 44, 102, 64, 94, 13, 81, 73, 35, 93,
					122, 102, 190, 153, 162, 233, 194, 101, 242, 24, 227, 213, 164, 94, 254, 4, 9,
					6, 232, 180, 228, 83, 87, 74, 245, 41, 119, 212, 15, 196, 85, 166, 211, 97,
					111, 105, 21, 241, 123, 211, 193, 6, 254, 125, 169, 108, 252, 85, 49, 31, 54,
					53, 79, 196, 5, 122, 206, 127, 226, 224, 70, 0,
				],
			},
			Cell {
				position: Position { row: 0, col: 1 },
				content: [
					165, 187, 167, 30, 116, 213, 60, 35, 8, 53, 187, 175, 212, 5, 173, 37, 229,
					147, 100, 43, 92, 133, 70, 203, 222, 218, 230, 148, 82, 175, 26, 252, 195, 81,
					70, 186, 215, 106, 224, 70, 86, 48, 206, 206, 246, 82, 189, 226, 83, 4, 110,
					41, 9, 29, 26, 180, 156, 219, 69, 155, 148, 49, 78, 25, 165, 147, 150, 253,
					251, 174, 49, 215, 191, 142, 169, 70, 17, 86, 218, 0,
				],
			},
			Cell {
				position: Position { row: 0, col: 2 },
				content: [
					177, 32, 13, 195, 108, 169, 237, 10, 35, 89, 89, 106, 35, 134, 95, 60, 105, 70,
					170, 107, 229, 23, 204, 171, 94, 248, 45, 163, 226, 161, 59, 96, 6, 144, 185,
					215, 203, 233, 130, 252, 180, 140, 194, 92, 87, 157, 221, 174, 247, 52, 138,
					161, 52, 83, 193, 255, 17, 235, 98, 10, 88, 241, 25, 186, 3, 174, 139, 200,
					128, 117, 255, 213, 200, 4, 46, 244, 219, 5, 131, 0,
				],
			},
		];
		let returned_cells: (Vec<Position>, Vec<Position>) = (
			vec![
				Position { row: 1, col: 0 },
				Position { row: 1, col: 1 },
				Position { row: 0, col: 1 },
				Position { row: 0, col: 0 },
			],
			vec![],
		);
		let verified_cells = returned_cells.clone().0;
		mock_client
			.expect_verify_cells()
			.withf(|x, _, _, _, _| *x == 2)
			.returning(move |_, _, _, _, _| {
				let returned_cells = returned_cells.clone();
				Ok(returned_cells)
			});
		mock_client
			.verify_cells(2, &dimensions, &cells, &commitments, &pp)
			.unwrap();
		mock_client
			.expect_store_confidence_in_db()
			.withf(|_, x| *x == 2)
			.returning(|_, _| Ok(true));
		mock_client
			.store_confidence_in_db(verified_cells.len() as u32, 2)
			.unwrap();
	}
}
