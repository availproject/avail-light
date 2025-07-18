//! Light (sync) client sampling and verification for blocks before latest finalized.
//!
//! Fetches and verifies previous blocks up to configured sync depth.
//!
//! # Flow
//!
//! * For each block, fetches block header from RPC and stores it into database
//! * Generate random cells for random data sampling
//! * Retrieve cell proofs from a) DHT and/or b) via RPC call from the node, in that order
//! * Verify proof using the received cells
//! * Calculate block confidence and store it in RocksDB
//! * Insert cells to to DHT for remote fetch
//!
//! # Notes
//!
//! In case RPC is disabled, RPC calls will be skipped.
use crate::{
	data::{
		AchievedSyncConfidenceKey, BlockHeaderKey, Database, IsSyncedKey, LatestSyncKey,
		VerifiedCellCountKey, VerifiedSyncHeaderKey,
	},
	network::{
		self,
		rpc::{self, Client as RpcClient},
	},
	types::{BlockRange, BlockVerified, SyncClientConfig},
	utils::{blake2_256, calculate_confidence, extract_kate},
};

use async_trait::async_trait;
use avail_rust::{AvailHeader, H256};
use codec::Encode;
use color_eyre::{eyre::WrapErr, Result};
use kate_recovery::commitments;
use mockall::automock;
use std::{ops::Range, time::Instant};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

#[async_trait]
#[automock]
pub trait Client {
	async fn get_header_by_block_number(&self, block_number: u32) -> Result<(AvailHeader, H256)>;
	fn is_confidence_stored(&self, block_number: u32) -> bool;
	fn store_verified_cell_count(&self, count: u32, block_number: u32);
	fn store_achieved_sync_confidence(&self, block_number: u32);
	fn store_verified_sync_header(&self, block_number: u32);
	fn store_latest_sync(&self, block_number: u32);
	fn store_is_synced(&self, is_synced: bool);
}

#[derive(Clone)]
pub struct SyncClient<T: Database + Sync> {
	db: T,
	rpc_client: RpcClient<T>,
}

impl<T: Database + Sync> SyncClient<T> {
	pub fn new(db: T, rpc_client: RpcClient<T>) -> Self {
		SyncClient { db, rpc_client }
	}
}

#[async_trait]
impl<T: Database + Sync> Client for SyncClient<T> {
	async fn get_header_by_block_number(&self, block_number: u32) -> Result<(AvailHeader, H256)> {
		if let Some(header) = self.db.get(BlockHeaderKey(block_number)) {
			let hash: H256 = Encode::using_encoded(&header, blake2_256).into();
			return Ok((header, hash));
		}

		let (header, hash) = self
			.rpc_client
			.get_header_by_block_number(block_number)
			.await
			.wrap_err("Failed to get block header from the RPC")?;

		self.db.put(BlockHeaderKey(block_number), header.clone());

		Ok((header, hash))
	}

	fn is_confidence_stored(&self, block_number: u32) -> bool {
		self.db.get(VerifiedCellCountKey(block_number)).is_some()
	}

	fn store_verified_cell_count(&self, count: u32, block_number: u32) {
		self.db.put(VerifiedCellCountKey(block_number), count)
	}

	fn store_achieved_sync_confidence(&self, block_number: u32) {
		// get the current BlockRange stored in DB under this key
		let mut block_range = self
			.db
			.get(AchievedSyncConfidenceKey)
			.unwrap_or_else(|| BlockRange::init(block_number));
		// mutate the value
		block_range.last = block_number;
		// store mutated value back in the DB
		self.db.put(AchievedSyncConfidenceKey, block_range);
	}

	fn store_verified_sync_header(&self, block_number: u32) {
		// get the current BlockRange stored in DB under this key
		let mut block_range = self
			.db
			.get(VerifiedSyncHeaderKey)
			.unwrap_or_else(|| BlockRange::init(block_number));
		// mutate the value
		block_range.last = block_number;
		// store mutated value back in the DB
		self.db.put(VerifiedSyncHeaderKey, block_range);
	}

	fn store_latest_sync(&self, block_number: u32) {
		self.db.put(LatestSyncKey, block_number)
	}

	fn store_is_synced(&self, is_synced: bool) {
		self.db.put(IsSyncedKey, is_synced)
	}
}

async fn process_block(
	client: &impl Client,
	network_client: &impl network::Client,
	header: AvailHeader,
	header_hash: H256,
	cfg: &SyncClientConfig,
	block_verified_sender: broadcast::Sender<BlockVerified>,
) -> Result<()> {
	let block_number = header.number;
	let begin = Instant::now();

	info!(block_number, elapsed = ?begin.elapsed(), "Synced block header");

	let (required, verified, unverified, mut block_verified) = match extract_kate(&header.extension)
	{
		None => {
			info!("Skipping block without header extension");

			return Ok(());
		},
		Some((_, _, _, commitment)) => {
			let block_verified = BlockVerified::try_from((&header, None))
				.wrap_err("converting to message failed")?;
			let Some(extension) = &block_verified.extension else {
				info!(
					block_number,
					"Skipping block: no valid extension (cannot derive dimensions)"
				);
				return Ok(());
			};

			if extension.dimensions.cols().get() <= 2 {
				error!(block_number, "More than 2 columns are required");
				return Ok(());
			}
			let target_grid_dimensions = block_verified
				.target_grid_dimensions
				.unwrap_or(extension.dimensions);
			let commitments = commitments::from_slice(&commitment)?;

			// now this is in `u64`
			let cell_count = rpc::cell_count_for_confidence(cfg.confidence);
			let positions = rpc::generate_random_cells(target_grid_dimensions, cell_count);

			let (fetched, unfetched, _fetch_stats) = network_client
				.fetch_verified(
					block_number,
					header_hash,
					extension.dimensions,
					&commitments,
					&positions,
				)
				.await?;
			(
				positions.len(),
				fetched.len(),
				unfetched.len(),
				block_verified,
			)
		},
	};

	if required > verified {
		error!(block_number, "Failed to fetch {} cells", unverified);
		return Ok(());
	}

	// write verified cell count into on-disk database
	client.store_verified_cell_count(verified.try_into()?, block_number);

	let confidence = Some(calculate_confidence(verified as u32));
	block_verified.confidence = confidence;

	if let Err(error) = block_verified_sender.send(block_verified) {
		error!("Cannot send block verified message: {error}");
	}

	Ok(())
}

/// Runs sync client.
///
/// # Arguments
///
/// * `cfg` - Sync client configuration
/// * `start_block` - Sync start block
/// * `end_block` - Sync end block
/// * `block_verified_sender` - Optional channel to send verified blocks
pub async fn run(
	client: impl Client,
	network_client: impl network::Client,
	cfg: SyncClientConfig,
	sync_range: Range<u32>,
	block_verified_sender: broadcast::Sender<BlockVerified>,
) {
	if sync_range.is_empty() {
		warn!("There are no blocks to sync for range {sync_range:?}");
		return;
	}
	let sync_blocks_depth = sync_range.len();
	if sync_blocks_depth >= 250 {
		warn!("In order to process {sync_blocks_depth} blocks behind latest block, connected nodes needs to be archive nodes!");
	}

	info!("Syncing block headers for {sync_range:?}");
	for block_number in sync_range {
		// TODO: This is still an ambiguous check since data fetch can fail.
		// We should write block status in DB explicitly.
		if client.is_confidence_stored(block_number) {
			continue;
		};

		let (header, header_hash) = match client.get_header_by_block_number(block_number).await {
			Ok(value) => value,
			Err(error) => {
				error!(block_number, "Cannot process block: {error:#}");
				continue;
			},
		};

		client.store_latest_sync(block_number);
		// TODO: Add proper header verification on sync
		client.store_verified_sync_header(block_number);

		// TODO: Should we handle unprocessed blocks differently?
		let block_verified_sender = block_verified_sender.clone();
		if let Err(error) = process_block(
			&client,
			&network_client,
			header,
			header_hash,
			&cfg,
			block_verified_sender,
		)
		.await
		{
			error!(block_number, "Cannot process block: {error:#}");
		} else {
			client.store_achieved_sync_confidence(block_number);
		}
	}

	if cfg.app_id.is_none() {
		client.store_is_synced(true);
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::types;
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
	use kate_recovery::{
		data::{Cell, SingleCell},
		matrix::Position,
	};
	use mockall::predicate::eq;

	fn default_header() -> AvailHeader {
		AvailHeader {
			parent_hash: hex!("2a75ea712b4b2c360cb7c0cdd806de4e9363ff7e37ce30788d487a258604dba3")
				.into(),
			number: 2,
			state_root: hex!("6f41d5a26a34f7bc3a09d4811b444c09daaebbd5c5d67c4525f42b3ed11bef86")
				.into(),
			extrinsics_root: hex!(
				"3027e34c2c75756c22770e6a3650ad68f3c9e44eed3c5ab4471742fe96678dae"
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
					commitment: vec![
						181, 10, 104, 251, 33, 171, 87, 192, 13, 195, 93, 127, 215, 78, 114, 192,
						95, 92, 167, 10, 49, 17, 20, 204, 222, 102, 70, 218, 173, 18, 30, 49, 232,
						10, 137, 187, 186, 216, 97, 140, 16, 33, 52, 56, 170, 208, 118, 242, 181,
						10, 104, 251, 33, 171, 87, 192, 13, 195, 93, 127, 215, 78, 114, 192, 95,
						92, 167, 10, 49, 17, 20, 204, 222, 102, 70, 218, 173, 18, 30, 49, 232, 10,
						137, 187, 186, 216, 97, 140, 16, 33, 52, 56, 170, 208, 118, 242,
					],
				},
				app_lookup: CompactDataLookup {
					size: 1,
					index: vec![],
				},
			}),
		}
	}

	#[tokio::test]
	pub async fn test_process_blocks_without_rpc() {
		let (block_tx, _) = broadcast::channel::<types::BlockVerified>(10);
		let cfg = SyncClientConfig {
			network_mode: types::NetworkMode::P2POnly,
			..Default::default()
		};
		let mut mock_network_client = network::MockClient::new();
		let mut mock_client = MockClient::new();
		let header = default_header();
		let header_hash: H256 =
			hex!("3767f8955d6f7306b1e55701b6316fa1163daa8d4cffdb05c3b25db5f5da1723").into();

		mock_network_client
			.expect_fetch_verified()
			.returning(move |_, _, _, _, positions| {
				let unfetched = vec![];
				let fetched: Vec<Cell> = vec![
					Cell::SingleCell(SingleCell {
						position: Position { row: 0, col: 0 },
						content: [
							183, 56, 112, 134, 157, 186, 15, 255, 245, 173, 188, 37, 165, 224, 226,
							80, 196, 137, 235, 233, 154, 4, 110, 142, 26, 95, 150, 132, 61, 23,
							202, 212, 101, 6, 235, 6, 102, 188, 206, 147, 36, 121, 128, 63, 240,
							37, 200, 236, 4, 44, 40, 4, 3, 0, 11, 35, 249, 222, 81, 135, 1, 128, 0,
							0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
						],
					}),
					Cell::SingleCell(SingleCell {
						position: Position { row: 0, col: 2 },
						content: [
							153, 31, 34, 70, 221, 239, 97, 236, 3, 172, 44, 167, 114, 117, 186,
							245, 171, 12, 70, 144, 204, 207, 82, 160, 29, 83, 245, 203, 40, 238,
							96, 131, 68, 96, 9, 136, 151, 88, 218, 72, 79, 55, 193, 228, 71, 193,
							120, 113, 48, 237, 151, 135, 246, 8, 251, 150, 106, 44, 29, 250, 250,
							54, 133, 203, 162, 73, 252, 32, 42, 175, 24, 166, 142, 72, 226, 150,
							163, 206, 115, 0,
						],
					}),
					Cell::SingleCell(SingleCell {
						position: Position { row: 1, col: 1 },
						content: [
							146, 211, 61, 65, 166, 68, 252, 65, 196, 167, 211, 64, 223, 151, 33,
							133, 67, 132, 59, 13, 224, 100, 55, 104, 180, 174, 17, 41, 151, 125,
							193, 80, 142, 140, 216, 97, 117, 60, 217, 44, 242, 7, 30, 204, 22, 197,
							12, 179, 88, 163, 102, 4, 54, 208, 14, 161, 193, 25, 34, 179, 35, 234,
							120, 131, 62, 53, 0, 54, 72, 49, 196, 234, 239, 65, 25, 159, 245, 38,
							193, 0,
						],
					}),
					Cell::SingleCell(SingleCell {
						position: Position { row: 0, col: 3 },
						content: [
							150, 6, 83, 12, 56, 17, 0, 225, 186, 238, 151, 181, 116, 1, 34, 240,
							174, 192, 98, 201, 60, 208, 50, 215, 90, 231, 2, 27, 17, 204, 140, 30,
							213, 253, 200, 176, 72, 98, 121, 25, 239, 76, 230, 154, 121, 246, 142,
							37, 85, 184, 201, 218, 107, 88, 0, 87, 199, 169, 98, 172, 4, 140, 151,
							65, 162, 162, 190, 205, 20, 95, 67, 114, 73, 59, 170, 52, 243, 140,
							237, 0,
						],
					}),
				];

				let stats = network::FetchStats::new(
					positions.len(),
					fetched.len(),
					Duration::from_secs(0),
					None,
				);
				Box::pin(async move { Ok((fetched, unfetched, stats)) })
			});
		mock_client
			.expect_is_confidence_stored()
			.with(eq(2))
			.returning(|_| true);
		mock_client
			.expect_store_verified_cell_count()
			.withf(move |_, block_number| *block_number == 2)
			.returning(move |_, _| ());
		process_block(
			&mock_client,
			&mock_network_client,
			header,
			header_hash,
			&cfg,
			block_tx,
		)
		.await
		.unwrap();
	}

	#[tokio::test]
	pub async fn test_process_blocks_with_rpc() {
		let (block_tx, _) = broadcast::channel::<types::BlockVerified>(10);
		let cfg = SyncClientConfig::default();
		let mut mock_network_client = network::MockClient::new();
		let mut mock_client = MockClient::new();
		let header = default_header();
		let header_hash: H256 =
			hex!("3767f8955d6f7306b1e55701b6316fa1163daa8d4cffdb05c3b25db5f5da1723").into();

		mock_network_client
			.expect_fetch_verified()
			.withf(|&x, _, _, _, _| x == 2)
			.returning(move |_, _, _, _, positions| {
				let unfetched = vec![Position { row: 0, col: 3 }];
				let dht_fetched: Vec<Cell> = vec![
					Cell::SingleCell(SingleCell {
						position: Position { row: 0, col: 0 },
						content: [
							183, 56, 112, 134, 157, 186, 15, 255, 245, 173, 188, 37, 165, 224, 226,
							80, 196, 137, 235, 233, 154, 4, 110, 142, 26, 95, 150, 132, 61, 23,
							202, 212, 101, 6, 235, 6, 102, 188, 206, 147, 36, 121, 128, 63, 240,
							37, 200, 236, 4, 44, 40, 4, 3, 0, 11, 35, 249, 222, 81, 135, 1, 128, 0,
							0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
						],
					}),
					Cell::SingleCell(SingleCell {
						position: Position { row: 0, col: 2 },
						content: [
							153, 31, 34, 70, 221, 239, 97, 236, 3, 172, 44, 167, 114, 117, 186,
							245, 171, 12, 70, 144, 204, 207, 82, 160, 29, 83, 245, 203, 40, 238,
							96, 131, 68, 96, 9, 136, 151, 88, 218, 72, 79, 55, 193, 228, 71, 193,
							120, 113, 48, 237, 151, 135, 246, 8, 251, 150, 106, 44, 29, 250, 250,
							54, 133, 203, 162, 73, 252, 32, 42, 175, 24, 166, 142, 72, 226, 150,
							163, 206, 115, 0,
						],
					}),
					Cell::SingleCell(SingleCell {
						position: Position { row: 1, col: 1 },
						content: [
							146, 211, 61, 65, 166, 68, 252, 65, 196, 167, 211, 64, 223, 151, 33,
							133, 67, 132, 59, 13, 224, 100, 55, 104, 180, 174, 17, 41, 151, 125,
							193, 80, 142, 140, 216, 97, 117, 60, 217, 44, 242, 7, 30, 204, 22, 197,
							12, 179, 88, 163, 102, 4, 54, 208, 14, 161, 193, 25, 34, 179, 35, 234,
							120, 131, 62, 53, 0, 54, 72, 49, 196, 234, 239, 65, 25, 159, 245, 38,
							193, 0,
						],
					}),
				];
				let rpc_fetched: Vec<Cell> = vec![Cell::SingleCell(SingleCell {
					position: Position { row: 0, col: 3 },
					content: [
						150, 6, 83, 12, 56, 17, 0, 225, 186, 238, 151, 181, 116, 1, 34, 240, 174,
						192, 98, 201, 60, 208, 50, 215, 90, 231, 2, 27, 17, 204, 140, 30, 213, 253,
						200, 176, 72, 98, 121, 25, 239, 76, 230, 154, 121, 246, 142, 37, 85, 184,
						201, 218, 107, 88, 0, 87, 199, 169, 98, 172, 4, 140, 151, 65, 162, 162,
						190, 205, 20, 95, 67, 114, 73, 59, 170, 52, 243, 140, 237, 0,
					],
				})];

				let stats = network::FetchStats::new(
					positions.len(),
					dht_fetched.len(),
					Duration::from_secs(0),
					Some((rpc_fetched.len(), Duration::from_secs(1))),
				);
				let fetched = [&dht_fetched[..], &rpc_fetched[..]].concat();
				Box::pin(async move { Ok((fetched, unfetched, stats)) })
			});

		mock_client
			.expect_store_verified_cell_count()
			.withf(move |_, block_number| *block_number == 2)
			.returning(move |_, _| ());
		process_block(
			&mock_client,
			&mock_network_client,
			header,
			header_hash,
			&cfg,
			block_tx,
		)
		.await
		.unwrap();
	}
}
