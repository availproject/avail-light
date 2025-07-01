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

use avail_rust::{AvailHeader, H256};
use codec::Encode;
use color_eyre::Result;
use kate_recovery::commitments;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, info};
#[cfg(target_arch = "wasm32")]
use {tokio_with_wasm::alias as tokio, web_time::Instant};

use crate::{
	data::{
		AchievedConfidenceKey, BlockHeaderKey, Database, DhtFetchedPercentageKey,
		VerifiedCellCountKey,
	},
	network::{
		self,
		rpc::{self, OutputEvent as RpcEvent},
	},
	shutdown::Controller,
	types::{self, BlockRange, BlockVerified, ClientChannels, Delay},
	utils::{blake2_256, calculate_confidence, extract_kate, is_flush_cycle},
};

#[derive(Debug)]
pub enum OutputEvent {
	RecordBlockProcessingDelay(f64),
	CountSessionBlocks,
	RecordBlockHeight(u32),
	RecordDHTStats {
		fetched: f64,
		fetched_percentage: f64,
		fetch_duration: f64,
	},
	RecordRPCFetched(f64),
	RecordRPCFetchDuration(f64),
	RecordBlockConfidence(f64),
}

pub async fn process_block(
	db: impl Database,
	network_client: &impl network::Client,
	confidence: f64,
	header: AvailHeader,
	received_at: Instant,
	event_sender: UnboundedSender<OutputEvent>,
) -> Result<Option<BlockVerified>> {
	let block_number = header.number;
	let header_hash: H256 = Encode::using_encoded(&header, blake2_256).into();

	event_sender.send(OutputEvent::CountSessionBlocks)?;
	event_sender.send(OutputEvent::RecordBlockHeight(block_number))?;

	info!(
		{ block_number, block_delay = received_at.elapsed().as_secs()},
		"Processing finalized block",
	);

	let (mut block_verified, required, verified, unverified) = match extract_kate(&header.extension)
	{
		None => {
			info!("Skipping block without header extension");
			// get current currently stored Achieved Confidence
			let mut achieved_confidence = db
				.get(AchievedConfidenceKey)
				.unwrap_or_else(|| BlockRange::init(block_number));

			achieved_confidence.last = block_number;
			db.put(AchievedConfidenceKey, achieved_confidence);
			db.put(BlockHeaderKey(block_number), header);

			return Ok(None);
		},
		Some((_, _, _, commitment)) => {
			let Ok(block_verified) = types::BlockVerified::try_from((&header, Some(confidence)))
			else {
				error!("Cannot create verified block from header");
				return Ok(None);
			};

			let Some(extension) = &block_verified.extension else {
				info!(
					block_number,
					"Skipping block: no valid extension (cannot derive dimensions)"
				);
				return Ok(None);
			};

			if extension.dimensions.cols().get() <= 2 {
				error!(block_number, "More than 2 columns are required");
				return Ok(None);
			}

			let commitments = commitments::from_slice(&commitment)?;
			let cell_count = rpc::cell_count_for_confidence(confidence);
			let target_grid_dimensions = block_verified
				.target_grid_dimensions
				.unwrap_or(extension.dimensions);
			let positions = rpc::generate_random_cells(target_grid_dimensions, cell_count);

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
					extension.dimensions,
					&commitments,
					&positions,
				)
				.await?;

			if is_flush_cycle(block_number) {
				db.put(DhtFetchedPercentageKey, fetch_stats.dht_fetched_percentage);
			}

			event_sender.send(OutputEvent::RecordDHTStats {
				fetched: fetch_stats.dht_fetched,
				fetched_percentage: fetch_stats.dht_fetched_percentage,
				fetch_duration: fetch_stats.dht_fetch_duration,
			})?;

			if let Some(rpc_fetched) = fetch_stats.rpc_fetched {
				event_sender.send(OutputEvent::RecordRPCFetched(rpc_fetched))?;
			}

			if let Some(rpc_fetch_duration) = fetch_stats.rpc_fetch_duration {
				event_sender.send(OutputEvent::RecordRPCFetchDuration(rpc_fetch_duration))?;
			}
			(
				block_verified,
				positions.len(),
				fetched.len(),
				unfetched.len(),
			)
		},
	};

	if required > verified {
		error!(block_number, "Failed to fetch {} cells", unverified);
		return Ok(None);
	}

	// write Verified Cell Count into on-disk db
	db.put(VerifiedCellCountKey(block_number), verified as u32);

	// get currently stored Achieved Confidence
	let mut achieved_confidence = db
		.get(AchievedConfidenceKey)
		.unwrap_or_else(|| BlockRange::init(block_number));

	achieved_confidence.last = block_number;

	db.put(AchievedConfidenceKey, achieved_confidence);

	let confidence = calculate_confidence(verified as u32);
	info!(
		block_number,
		"confidence" = confidence,
		"Confidence factor: {}",
		confidence
	);
	event_sender.send(OutputEvent::RecordBlockConfidence(confidence))?;
	block_verified.confidence = Some(confidence);

	// push latest mined block's header into column family specified
	// for keeping block headers, to be used
	// later for verifying DHT stored data
	//
	// @note this same data store is also written to in
	// another competing thread, which syncs all block headers
	// in range [0, LATEST], where LATEST = latest block number
	// when this process started
	db.put(BlockHeaderKey(block_number), header);

	Ok(Some(block_verified))
}

/// Runs light client.
///
/// # Arguments
///
/// * `light_client` - Light client implementation
/// * `metrics` - Metrics registry
/// * `state` - Processed blocks state
/// * `channels` - Communication channels
/// * `shutdown` - Shutdown controller
#[allow(clippy::too_many_arguments)]
pub async fn run(
	db: impl Database + Clone,
	network_client: impl network::Client,
	confidence: f64,
	block_processing_delay: Delay,
	mut channels: ClientChannels,
	shutdown: Controller<String>,
	event_sender: UnboundedSender<OutputEvent>,
) {
	info!("Starting light client...");

	loop {
		let event_sender = event_sender.clone();

		let event = match channels.rpc_event_receiver.recv().await {
			Ok(event) => event,
			Err(error) => {
				error!("Cannot receive message: {error}");
				return;
			},
		};

		// Only process HeaderUpdate events, skip others
		if let RpcEvent::HeaderUpdate {
			header,
			received_at,
			..
		} = event
		{
			if let Some(seconds) = block_processing_delay.sleep_duration(received_at) {
				if let Err(error) = event_sender.send(OutputEvent::RecordBlockProcessingDelay(
					seconds.as_secs_f64(),
				)) {
					error!("Cannot send OutputEvent message: {error}");
				}
				info!("Sleeping for {seconds:?} seconds");
				tokio::time::sleep(seconds).await;
			}

			let process_block_result = process_block(
				db.clone(),
				&network_client,
				confidence,
				header.clone(),
				received_at,
				event_sender,
			)
			.await;

			let client_msg = match process_block_result {
				Ok(Some(blk_verified)) => blk_verified,
				Ok(None) => continue,
				Err(error) => {
					error!("Cannot process block: {error}");
					let _ = shutdown.trigger_shutdown(format!("Cannot process block: {error:#}"));
					return;
				},
			};

			// Notify dht-based application client
			// that the newly mined block has been received
			if let Err(error) = channels.block_sender.send(client_msg) {
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
		data,
		network::rpc::{cell_count_for_confidence, CELL_COUNT_99_99},
	};
	use avail_rust::{
		avail::runtime_types::avail_core::{
			data_lookup::compact::CompactDataLookup,
			header::extension::{v3::HeaderExtension, HeaderExtension::V3},
			kate_commitment::v3::KateCommitment,
		},
		subxt::config::substrate::Digest,
		AvailHeader,
	};
	use hex_literal::hex;
	use kate_recovery::{data::Cell, matrix::Position};
	use test_case::test_case;
	use tokio::sync::mpsc;

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
		let db = data::MemoryDB::default();
		let cells_fetched: Vec<Cell> = vec![];
		let cells_unfetched = [
			Position { row: 1, col: 3 },
			Position { row: 0, col: 0 },
			Position { row: 1, col: 2 },
			Position { row: 0, col: 1 },
		]
		.to_vec();
		let header = AvailHeader {
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
		let recv = Instant::now();
		let (sender, _receiver) = mpsc::unbounded_channel::<OutputEvent>();
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

		process_block(db, &mock_network_client, 99.9, header, recv, sender)
			.await
			.unwrap();
	}
}
