//! Fat client for fetching the data partition and inserting into the DHT.
//!
//! # Flow
//!
//! * Fetches assigned block partition when finalized header is available and
//! * inserts data rows and cells to to DHT for remote fetch.
//!
//! # Notes
//!
//! In case delay is configured, block processing is delayed for configured time.

use async_trait::async_trait;
#[cfg(not(feature = "multiproof"))]
use avail_rust::kate_recovery::data::{self, SingleCell};
#[cfg(feature = "multiproof")]
use avail_rust::{kate_recovery::data::MultiProofCell, utils::generate_multiproof_grid_dims};
use avail_rust::{
	kate_recovery::{
		data::Cell,
		matrix::{Dimensions, Partition, Position, RowIndex},
	},
	AvailHeader, H256,
};
use codec::Encode;
use color_eyre::{eyre::WrapErr, Result};
use futures::future::join_all;
use mockall::automock;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info};

#[cfg(feature = "multiproof")]
use crate::types::MULTI_PROOF_CELL_DIMS;
use crate::{
	data::{BlockHeaderKey, Database},
	network::{
		p2p::Client as P2pClient,
		rpc::{Client as RpcClient, OutputEvent as RpcEvent},
	},
	shutdown::Controller,
	types::{block_matrix_partition_format, BlockVerified, ClientChannels, Delay},
	utils::{blake2_256, extract_kate},
};

#[async_trait]
#[automock]
pub trait Client {
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> Result<()>;
	async fn insert_rows_into_dht(&self, block: u32, rows: Vec<(RowIndex, Vec<u8>)>) -> Result<()>;
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>>;
}

#[derive(Clone)]
pub struct FatClient<T: Database> {
	p2p_client: P2pClient,
	rpc_client: RpcClient<T>,
}

pub fn new(
	p2p_client: P2pClient,
	rpc_client: RpcClient<impl Database>,
) -> FatClient<impl Database> {
	FatClient {
		p2p_client,
		rpc_client,
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
	/// Fraction and number of the block matrix part to fetch (e.g. 2/20 means second 1/20 part of a matrix) (default: 1/1)
	#[serde(with = "block_matrix_partition_format")]
	pub block_matrix_partition: Partition,
	/// Maximum number of cells per request for proof queries (default: 30).
	pub max_cells_per_rpc: usize,
	/// Number of parallel queries for cell fetching via RPC from node (default: 8).
	pub query_proof_rpc_parallel_tasks: usize,
}

pub const ENTIRE_BLOCK: Partition = Partition {
	number: 1,
	fraction: 1,
};

impl Default for Config {
	fn default() -> Self {
		Self {
			block_matrix_partition: ENTIRE_BLOCK,
			max_cells_per_rpc: 30,
			query_proof_rpc_parallel_tasks: 8,
		}
	}
}

#[async_trait]
impl<T: Database + Sync> Client for FatClient<T> {
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> Result<()> {
		self.p2p_client.insert_cells_into_dht(block, cells).await
	}

	async fn insert_rows_into_dht(&self, block: u32, rows: Vec<(RowIndex, Vec<u8>)>) -> Result<()> {
		self.p2p_client.insert_rows_into_dht(block, rows).await
	}

	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>> {
		#[cfg(feature = "multiproof")]
		{
			let cells: Vec<MultiProofCell> = self
				.rpc_client
				.request_kate_multi_proof(hash, positions)
				.await?;
			Ok(cells.into_iter().map(Cell::MultiProofCell).collect())
		}

		#[cfg(not(feature = "multiproof"))]
		{
			let cells: Vec<SingleCell> =
				self.rpc_client.request_kate_proof(hash, positions).await?;
			Ok(cells.into_iter().map(Cell::SingleCell).collect())
		}
	}
}

pub enum OutputEvent {
	CountSessionBlocks,
	RecordBlockHeight(u32),
	RecordRpcCallDuration(f64),
	RecordBlockProcessingDelay(f64),
}

pub async fn process_block(
	client: &impl Client,
	db: impl Database,
	cfg: &Config,
	header: &AvailHeader,
	received_at: Instant,
	event_sender: UnboundedSender<OutputEvent>,
) -> Result<()> {
	event_sender.send(OutputEvent::CountSessionBlocks)?;
	event_sender.send(OutputEvent::RecordBlockHeight(header.number))?;

	let block_number = header.number;
	let header_hash: H256 = Encode::using_encoded(header, blake2_256).into();
	let block_delay = received_at.elapsed().as_secs();
	info!(block_number, block_delay, "Processing finalized block",);

	let Some((rows, cols, _, _)) = extract_kate(&header.extension) else {
		info!(block_number, "Skipping block without header extension");
		return Ok(());
	};
	let Some(dimensions) = Dimensions::new(rows, cols) else {
		info!(
			block_number,
			"Skipping block with invalid dimensions {rows}x{cols}",
		);
		return Ok(());
	};

	if dimensions.cols().get() <= 2 {
		error!(block_number, "More than 2 columns are required");
		return Ok(());
	}

	// push latest mined block's header into column family specified
	// for keeping block headers, to be used
	// later for verifying DHT stored data
	//
	// @note this same data store is also written to in
	// another competing thread, which syncs all block headers
	// in range [0, LATEST], where LATEST = latest block number
	// when this process started
	db.put(BlockHeaderKey(block_number), header.clone());

	let positions: Vec<Position> = {
		#[cfg(feature = "multiproof")]
		{
			let Some(multiproof_cell_dims) =
				Dimensions::new(MULTI_PROOF_CELL_DIMS.0, MULTI_PROOF_CELL_DIMS.1)
			else {
				info!(
					block_number,
					"Skipping block with invalid multiproof cell dimensions",
				);
				return Ok(());
			};

			let Some(target_multiproof_grid_dims) =
				generate_multiproof_grid_dims(multiproof_cell_dims, dimensions)
			else {
				info!(
					block_number,
					"Skipping block with invalid target multiproof grid dimensions",
				);
				return Ok(());
			};

			target_multiproof_grid_dims
				.iter_mcell_partition_positions(&cfg.block_matrix_partition)
				.collect()
		}

		#[cfg(not(feature = "multiproof"))]
		{
			dimensions
				.iter_extended_partition_positions(&cfg.block_matrix_partition)
				.collect()
		}
	};

	let begin = Instant::now();
	let get_kate_proof = |&n| client.get_kate_proof(header_hash, n);
	let Partition { number, fraction } = cfg.block_matrix_partition;
	info!(
		block_number,
		"partition_cells_requested" = positions.len(),
		"Fetching partition ({number}/{fraction}) from RPC",
	);
	let mut rpc_fetched: Vec<Cell> = vec![];
	let rpc_batches = positions.chunks(cfg.max_cells_per_rpc).collect::<Vec<_>>();
	let parallel_batches = rpc_batches
		.chunks(cfg.query_proof_rpc_parallel_tasks)
		.map(|batch| join_all(batch.iter().map(get_kate_proof)))
		.collect::<Vec<_>>();

	for batch in parallel_batches {
		for (i, result) in batch.await.into_iter().enumerate() {
			let batch_rpc_fetched =
				result.wrap_err(format!("Failed to fetch cells from node RPC at batch {i}"))?;

			if let Err(e) = client
				.insert_cells_into_dht(block_number, batch_rpc_fetched.clone())
				.await
			{
				debug!("Error inserting cells into DHT: {e}");
			}

			rpc_fetched.extend(batch_rpc_fetched);
		}
	}

	let partition_rpc_retrieve_time_elapsed = begin.elapsed();
	let partition_rpc_cells_fetched = rpc_fetched.len();
	info!(
		block_number,
		?partition_rpc_retrieve_time_elapsed,
		partition_rpc_cells_fetched,
		"Partition cells received from RPC",
	);

	event_sender.send(OutputEvent::RecordRpcCallDuration(
		partition_rpc_retrieve_time_elapsed.as_secs_f64(),
	))?;

	#[cfg(not(feature = "multiproof"))]
	if rpc_fetched.len() >= dimensions.cols().get() as usize {
		let cells: Vec<SingleCell> = rpc_fetched
			.into_iter()
			.filter(|c| !c.position().is_extended())
			.filter_map(|c| SingleCell::try_from(c).ok())
			.collect();

		let data_cells: Vec<&SingleCell> = cells.iter().collect();
		let data_rows = data::rows(dimensions, &data_cells);

		if let Err(e) = client.insert_rows_into_dht(block_number, data_rows).await {
			debug!("Error inserting rows into DHT: {e}");
		}
	} else {
		// NOTE: Often rows will not be push into DHT,
		// but that's ok, because only application clients requests them
		info!("No rows has been inserted into DHT since partition size is less than one row.")
	}

	Ok(())
}

/// Runs the fat client.
///
/// # Arguments
///
/// * `fat_client` - Fat client implementation
/// * `metrics` -  Metrics registry
/// * `channels` - Communication channels
/// * `partition` - Assigned fat client partition
/// * `shutdown` - Shutdown controller
pub async fn run(
	client: impl Client,
	db: impl Database + Clone,
	cfg: Config,
	block_processing_delay: Option<Duration>,
	event_sender: UnboundedSender<OutputEvent>,
	mut channels: ClientChannels,
	shutdown: Controller<String>,
) {
	info!("Starting fat client...");
	let delay = Delay(block_processing_delay);

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
			if let Some(seconds) = delay.sleep_duration(received_at) {
				info!("Sleeping for {seconds:?} seconds");
				if let Err(error) = event_sender.send(OutputEvent::RecordBlockProcessingDelay(
					seconds.as_secs_f64(),
				)) {
					error!("Failed to send RecordBlockProcessingDelay event: {error}");
				}
				tokio::time::sleep(seconds).await;
			}

			if let Err(error) = process_block(
				&client,
				db.clone(),
				&cfg,
				&header,
				received_at,
				event_sender,
			)
			.await
			{
				error!("Cannot process block: {error}");
				let _ = shutdown.trigger_shutdown(format!("Cannot process block: {error:#}"));
				return;
			};

			let Ok(client_msg) = BlockVerified::try_from((header, None)) else {
				error!("Cannot create message from header");
				continue;
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
	use super::*;
	use crate::data;
	use avail_rust::{
		avail::runtime_types::avail_core::{
			data_lookup::compact::CompactDataLookup,
			header::extension::{v3::HeaderExtension, HeaderExtension::V3},
			kate_commitment::v3::KateCommitment,
		},
		kate_recovery::data::SingleCell,
		subxt::config::substrate::Digest,
		AvailHeader,
	};
	use hex_literal::hex;
	use tokio::sync::mpsc;

	fn default_header() -> AvailHeader {
		AvailHeader {
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
		}
	}

	const DEFAULT_CELLS: [Cell; 4] = [
		Cell::SingleCell(SingleCell {
			position: Position { row: 0, col: 2 },
			content: [
				183, 215, 10, 175, 218, 48, 236, 18, 30, 163, 215, 125, 205, 130, 176, 227, 133,
				157, 194, 35, 153, 144, 141, 7, 208, 133, 170, 79, 27, 176, 202, 22, 111, 63, 107,
				147, 93, 44, 82, 137, 78, 32, 161, 175, 214, 152, 125, 50, 247, 52, 138, 161, 52,
				83, 193, 255, 17, 235, 98, 10, 88, 241, 25, 186, 3, 174, 139, 200, 128, 117, 255,
				213, 200, 4, 46, 244, 219, 5, 131, 0,
			],
		}),
		Cell::SingleCell(SingleCell {
			position: Position { row: 1, col: 1 },
			content: [
				172, 213, 85, 167, 89, 247, 11, 125, 149, 170, 217, 222, 86, 157, 11, 20, 154, 21,
				173, 247, 193, 99, 189, 7, 225, 80, 156, 94, 83, 213, 217, 185, 113, 187, 112, 20,
				170, 120, 50, 171, 52, 178, 209, 244, 158, 24, 129, 236, 83, 4, 110, 41, 9, 29, 26,
				180, 156, 219, 69, 155, 148, 49, 78, 25, 165, 147, 150, 253, 251, 174, 49, 215,
				191, 142, 169, 70, 17, 86, 218, 0,
			],
		}),
		Cell::SingleCell(SingleCell {
			position: Position { row: 0, col: 3 },
			content: [
				132, 180, 92, 81, 128, 83, 245, 59, 206, 224, 200, 137, 236, 113, 109, 216, 161,
				248, 236, 252, 252, 22, 140, 107, 203, 161, 33, 18, 100, 189, 157, 58, 7, 183, 146,
				75, 57, 220, 84, 106, 203, 33, 142, 10, 130, 99, 90, 38, 85, 166, 211, 97, 111,
				105, 21, 241, 123, 211, 193, 6, 254, 125, 169, 108, 252, 85, 49, 31, 54, 53, 79,
				196, 5, 122, 206, 127, 226, 224, 70, 0,
			],
		}),
		Cell::SingleCell(SingleCell {
			position: Position { row: 1, col: 3 },
			content: [
				132, 180, 92, 81, 128, 83, 245, 59, 206, 224, 200, 137, 236, 113, 109, 216, 161,
				248, 236, 252, 252, 22, 140, 107, 203, 161, 33, 18, 100, 189, 157, 58, 7, 183, 146,
				75, 57, 220, 84, 106, 203, 33, 142, 10, 130, 99, 90, 38, 85, 166, 211, 97, 111,
				105, 21, 241, 123, 211, 193, 6, 254, 125, 169, 108, 252, 85, 49, 31, 54, 53, 79,
				196, 5, 122, 206, 127, 226, 224, 70, 0,
			],
		}),
	];

	#[tokio::test]
	async fn process_block_successful() {
		let db = data::MemoryDB::default();
		let mut mock_client = MockClient::new();
		let cell_variants: Vec<Cell> = DEFAULT_CELLS.into_iter().map(Into::into).collect();

		mock_client.expect_get_kate_proof().returning(move |_, _| {
			Box::pin({
				let cells = cell_variants.clone();
				async move { Ok(cells.to_vec()) }
			})
		});
		mock_client
			.expect_insert_rows_into_dht()
			.returning(|_, _| Box::pin(async move { Ok(()) }));
		mock_client
			.expect_insert_cells_into_dht()
			.returning(|_, _| Box::pin(async move { Ok(()) }));
		let (sender, _receiver) = mpsc::unbounded_channel::<OutputEvent>();

		process_block(
			&mock_client,
			db,
			&Config::default(),
			&default_header(),
			Instant::now(),
			sender,
		)
		.await
		.unwrap();
	}
}
