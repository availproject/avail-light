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

use avail_subxt::{primitives::Header, utils::H256};
use codec::Encode;
use color_eyre::{eyre::WrapErr, Result};
use kate_recovery::{commitments, matrix::Dimensions};
use sp_core::blake2_256;
use std::{
	sync::{Arc, Mutex},
	time::Instant,
};
use tracing::{error, info};

use crate::{
	data::{Database, Key},
	network::{
		self,
		rpc::{self, Event},
	},
	shutdown::Controller,
	telemetry::{MetricCounter, MetricValue, Metrics},
	types::{self, ClientChannels, LightClientConfig, OptionBlockRange, State},
	utils::{calculate_confidence, extract_kate},
};

pub async fn process_block(
	db: impl Database,
	network_client: &impl network::Client,
	metrics: &Arc<impl Metrics>,
	cfg: &LightClientConfig,
	header: Header,
	received_at: Instant,
	state: Arc<Mutex<State>>,
) -> Result<Option<f64>> {
	metrics.count(MetricCounter::SessionBlock).await;
	metrics
		.record(MetricValue::TotalBlockNumber(header.number))
		.await?;

	let block_number = header.number;
	let header_hash: H256 = Encode::using_encoded(&header, blake2_256).into();

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

	if positions.len() > fetched.len() {
		error!(block_number, "Failed to fetch {} cells", unfetched.len());
		return Ok(None);
	}

	// write confidence factor into on-disk database
	db.put(Key::VerifiedCellCount(block_number), fetched.len() as u32)
		.wrap_err("Light Client failed to store Confidence Factor")?;

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
	db.put(Key::BlockHeader(block_number), header)
		.wrap_err("Light Client failed to store Block Header")?;

	Ok(Some(confidence))
}

/// Runs light client.
///
/// # Arguments
///
/// * `light_client` - Light client implementation
/// * `cfg` - Light client configuration
/// * `metrics` - Metrics registry
/// * `state` - Processed blocks state
/// * `channels` - Communication channels
/// * `shutdown` - Shutdown controller
pub async fn run(
	db: impl Database + Clone,
	network_client: impl network::Client,
	cfg: LightClientConfig,
	metrics: Arc<impl Metrics>,
	state: Arc<Mutex<State>>,
	mut channels: ClientChannels,
	shutdown: Controller<String>,
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
			db.clone(),
			&network_client,
			&metrics,
			&cfg,
			header.clone(),
			received_at,
			state.clone(),
		)
		.await;
		let confidence = match process_block_result {
			Ok(confidence) => confidence,
			Err(error) => {
				error!("Cannot process block: {error}");
				let _ = shutdown.trigger_shutdown(format!("Cannot process block: {error:#}"));
				return;
			},
		};

		let Ok(client_msg) = types::BlockVerified::try_from((header, confidence)) else {
			error!("Cannot create message from header");
			continue;
		};

		// notify dht-based application client
		// that newly mined block has been received
		if let Err(error) = channels.block_sender.send(client_msg) {
			error!("Cannot send block verified message: {error}");
			continue;
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::{
		data::mem_db,
		network::rpc::{cell_count_for_confidence, CELL_COUNT_99_99},
		telemetry,
		types::RuntimeConfig,
	};
	use avail_subxt::{
		api::runtime_types::avail_core::{
			data_lookup::compact::CompactDataLookup,
			header::extension::{v3::HeaderExtension, HeaderExtension::V3},
			kate_commitment::v3::KateCommitment,
		},
		config::substrate::Digest,
	};
	use hex_literal::hex;
	use kate_recovery::{data::Cell, matrix::Position};
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
		let mut mock_network_client = network::MockClient::new();
		let db = mem_db::MemoryDB::default();
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
			extension: V3(HeaderExtension {
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
				);
				Box::pin(async move { Ok((fetched, unfetched, stats)) })
			});

		let mut mock_metrics = telemetry::MockMetrics::new();
		mock_metrics.expect_count().returning(|_| ());
		mock_metrics.expect_record().returning(|_| Ok(()));
		mock_metrics.expect_set_multiaddress().returning(|_| ());
		process_block(
			db,
			&mock_network_client,
			&Arc::new(mock_metrics),
			&cfg,
			header,
			recv,
			state,
		)
		.await
		.unwrap();
	}
}
