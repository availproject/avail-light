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

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use avail_subxt::{avail, primitives::Header as DaHeader, utils::H256};
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use kate_recovery::{commitments, matrix::Dimensions};
use kate_recovery::{data::Cell, matrix::Position};
use mockall::automock;
use rocksdb::DB;
use std::{
	sync::{Arc, Mutex},
	time::Instant,
};
use tokio::sync::mpsc::Sender;
use tracing::{error, info, warn};

use crate::{
	data::{
		is_block_header_in_db, is_confidence_in_db, store_block_header_in_db,
		store_confidence_in_db,
	},
	network::Client,
	proof, rpc,
	types::{BlockVerified, State, SyncClientConfig},
	utils::{extract_app_lookup, extract_kate},
};

#[async_trait]
#[automock]
pub trait SyncClient {
	fn block_header_in_db(&self, block_number: u32) -> Result<bool>;
	async fn get_header_by_block_number(&self, block_number: u32) -> Result<(DaHeader, H256)>;
	fn store_block_header_in_db(&self, header: DaHeader, block_number: u32) -> Result<()>;
	fn is_confidence_in_db(&self, block_number: u32) -> Result<bool>;
	fn store_confidence_in_db(&self, count: u32, block_number: u32) -> Result<()>;
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>>;
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> f32;
	async fn fetch_cells_from_dht(
		&self,
		positions: &[Position],
		block_number: u32,
	) -> (Vec<Cell>, Vec<Position>);
	fn get_client(&self) -> avail::Client;
}
#[derive(Clone)]
struct SyncClientImpl {
	db: Arc<DB>,
	network_client: Client,
	rpc_client: avail::Client,
}

pub fn new(db: Arc<DB>, network_client: Client, rpc_client: avail::Client) -> impl SyncClient {
	SyncClientImpl {
		db,
		network_client,
		rpc_client,
	}
}

#[async_trait]
impl SyncClient for SyncClientImpl {
	fn block_header_in_db(&self, block_number: u32) -> Result<bool> {
		is_block_header_in_db(self.db.clone(), block_number)
			.context("Failed to check if block header is in DB")
	}

	async fn get_header_by_block_number(&self, block_number: u32) -> Result<(DaHeader, H256)> {
		rpc::get_header_by_block_number(&self.rpc_client, block_number)
			.await
			.with_context(|| format!("Failed to get block {block_number} by block number"))
	}

	fn store_block_header_in_db(&self, header: DaHeader, block_number: u32) -> Result<()> {
		store_block_header_in_db(self.db.clone(), block_number, &header)
			.context("Failed to store block header in DB")
	}

	fn is_confidence_in_db(&self, block_number: u32) -> Result<bool> {
		is_confidence_in_db(self.db.clone(), block_number)
			.context("Failed to check if confidence is in DB")
	}

	fn store_confidence_in_db(&self, count: u32, block_number: u32) -> Result<()> {
		store_confidence_in_db(self.db.clone(), block_number, count)
			.context("Failed to store confidence in DB")
	}

	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>> {
		rpc::get_kate_proof(&self.rpc_client, hash, positions).await
	}

	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> f32 {
		self.network_client
			.insert_cells_into_dht(block, cells)
			.await
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
	fn get_client(&self) -> avail::Client {
		self.rpc_client.clone()
	}
}

async fn process_block(
	sync_client: &impl SyncClient,
	block_number: u32,
	cfg: &SyncClientConfig,
	pp: Arc<PublicParameters>,
	block_verified_sender: Option<Sender<BlockVerified>>,
) -> Result<()> {
	if sync_client
		.block_header_in_db(block_number)
		.context("Failed to check if block header is in DB")?
	{
		// TODO: If block header storing fails, that block will be skipped upon restart
		// Better option would be to check for confidence
		return Ok(());
	};

	// if block header look up fails, only then comes here for
	// fetching and storing block header as part of (light weight)
	// syncing process
	let begin = Instant::now();

	let (header, header_hash) = sync_client.get_header_by_block_number(block_number).await?;

	let app_lookup = extract_app_lookup(&header.extension);

	info!(block_number, "App index {:?}", app_lookup);

	sync_client
		.store_block_header_in_db(header.clone(), block_number)
		.context("Failed to store block header in DB")?;

	info!(block_number, elapsed = ?begin.elapsed(), "Synced block header");

	// If it's found that this certain block is not verified
	// then it'll be verified now
	if sync_client
		.is_confidence_in_db(block_number)
		.context("Failed to check if confidence is in DB")?
	{
		return Ok(());
	};

	let begin = Instant::now();

	let (rows, cols, _, commitment) = extract_kate(&header.extension);
	let dimensions = Dimensions::new(rows, cols).context("Invalid dimensions")?;

	let commitments = commitments::from_slice(&commitment)?;

	// now this is in `u64`
	let cell_count = rpc::cell_count_for_confidence(cfg.confidence);
	let positions = rpc::generate_random_cells(dimensions, cell_count);

	let (dht_fetched, unfetched) = sync_client
		.fetch_cells_from_dht(&positions, block_number)
		.await;

	info!(
		block_number,
		"Number of cells fetched from DHT: {}",
		dht_fetched.len()
	);

	let rpc_fetched = if cfg.disable_rpc {
		vec![]
	} else {
		sync_client.get_kate_proof(header_hash, &unfetched).await?
	};

	info!(
		block_number,
		"Number of cells fetched from RPC: {}",
		rpc_fetched.len()
	);

	let mut cells = vec![];
	cells.extend(dht_fetched);
	cells.extend(rpc_fetched.clone());
	if positions.len() > cells.len() {
		return Err(anyhow!(
			"Failed to fetch {} cells",
			positions.len() - cells.len()
		));
	}

	let cells_len = cells.len();
	info!(block_number, "Fetched {cells_len} cells for verification");

	let (verified, _) = proof::verify(block_number, dimensions, &cells, &commitments, pp)?;

	info!(
		block_number,
		elapsed = ?begin.elapsed(),
		"Completed {cells_len} verification rounds",
	);

	// write confidence factor into on-disk database
	sync_client
		.store_confidence_in_db(verified.len().try_into()?, block_number)
		.context("Failed to store confidence in DB")?;

	let inserted_cells = sync_client
		.insert_cells_into_dht(block_number, rpc_fetched)
		.await;
	info!(block_number, "Cells inserted into DHT: {inserted_cells}");

	let client_msg = BlockVerified::try_from(header).context("converting to message failed")?;

	if let Some(ref channel) = block_verified_sender {
		if let Err(error) = channel.send(client_msg).await {
			error!("Cannot send block verified message: {error}");
		}
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
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
/// * `block_verified_sender` - Optional channel to send verified blocks
pub async fn run(
	sync_client: impl SyncClient,
	cfg: SyncClientConfig,
	start_block: u32,
	end_block: u32,
	pp: Arc<PublicParameters>,
	block_verified_sender: Option<Sender<BlockVerified>>,
	state: Arc<Mutex<State>>,
) {
	if start_block >= end_block {
		warn!("There are no blocks to sync from {start_block} to {end_block}");
		return;
	}
	let sync_blocks_depth = end_block - start_block;
	if sync_blocks_depth >= 250 {
		warn!("In order to process {sync_blocks_depth} blocks behind latest block, connected nodes needs to be archive nodes!");
	}

	info!("Syncing block headers from {start_block} to {end_block}");
	for block_number in start_block..=end_block {
		info!("Testing block {block_number}!");

		// TODO: Should we handle unprocessed blocks differently?
		let block_verified_sender = block_verified_sender.clone();
		let pp = pp.clone();
		if let Err(error) =
			process_block(&sync_client, block_number, &cfg, pp, block_verified_sender).await
		{
			error!(block_number, "Cannot process block: {error:#}");
		} else {
			state
				.lock()
				.unwrap()
				.set_sync_confidence_achieved(block_number);
		}
	}

	if block_verified_sender.is_none() {
		state.lock().unwrap().set_synced(true);
	}
}

#[cfg(test)]
mod tests {

	use super::*;
	use crate::types::{self, RuntimeConfig};
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
	use mockall::predicate::eq;
	use tokio::sync::mpsc::channel;

	#[tokio::test]
	pub async fn test_process_blocks_without_rpc() {
		let (block_tx, _) = channel::<types::BlockVerified>(10);
		let pp = Arc::new(testnet::public_params(1024));
		let mut cfg = SyncClientConfig::from(&RuntimeConfig::default());
		cfg.disable_rpc = true;
		let mut mock_client = MockSyncClient::new();
		let header: DaHeader = DaHeader {
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
				app_lookup: CompactDataLookup {
					size: 1,
					index: vec![],
				},
			}),
		};
		let header_hash: H256 =
			hex!("3767f8955d6f7306b1e55701b6316fa1163daa8d4cffdb05c3b25db5f5da1723").into();
		mock_client
			.expect_block_header_in_db()
			.with(eq(42))
			.returning(|_| Ok(false));

		mock_client
			.expect_get_header_by_block_number()
			.with(eq(42))
			.returning(move |_| {
				let header = header.clone();

				Box::pin(async move { Ok((header, header_hash)) })
			});
		mock_client
			.expect_store_block_header_in_db()
			.withf(|_, x| *x == 42)
			.returning(|_, _| Ok(()));
		mock_client
			.expect_is_confidence_in_db()
			.with(eq(42))
			.returning(|_| Ok(true));
		mock_client
			.expect_fetch_cells_from_dht()
			.withf(|_, x: &u32| *x == 42)
			.returning(|_, _| {
				let dht = vec![];
				let fetch: Vec<Cell> = vec![
					Cell {
						position: Position { row: 0, col: 0 },
						content: [
							183, 56, 112, 134, 157, 186, 15, 255, 245, 173, 188, 37, 165, 224, 226,
							80, 196, 137, 235, 233, 154, 4, 110, 142, 26, 95, 150, 132, 61, 23,
							202, 212, 101, 6, 235, 6, 102, 188, 206, 147, 36, 121, 128, 63, 240,
							37, 200, 236, 4, 44, 40, 4, 3, 0, 11, 35, 249, 222, 81, 135, 1, 128, 0,
							0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
						],
					},
					Cell {
						position: Position { row: 0, col: 2 },
						content: [
							153, 31, 34, 70, 221, 239, 97, 236, 3, 172, 44, 167, 114, 117, 186,
							245, 171, 12, 70, 144, 204, 207, 82, 160, 29, 83, 245, 203, 40, 238,
							96, 131, 68, 96, 9, 136, 151, 88, 218, 72, 79, 55, 193, 228, 71, 193,
							120, 113, 48, 237, 151, 135, 246, 8, 251, 150, 106, 44, 29, 250, 250,
							54, 133, 203, 162, 73, 252, 32, 42, 175, 24, 166, 142, 72, 226, 150,
							163, 206, 115, 0,
						],
					},
					Cell {
						position: Position { row: 1, col: 1 },
						content: [
							146, 211, 61, 65, 166, 68, 252, 65, 196, 167, 211, 64, 223, 151, 33,
							133, 67, 132, 59, 13, 224, 100, 55, 104, 180, 174, 17, 41, 151, 125,
							193, 80, 142, 140, 216, 97, 117, 60, 217, 44, 242, 7, 30, 204, 22, 197,
							12, 179, 88, 163, 102, 4, 54, 208, 14, 161, 193, 25, 34, 179, 35, 234,
							120, 131, 62, 53, 0, 54, 72, 49, 196, 234, 239, 65, 25, 159, 245, 38,
							193, 0,
						],
					},
					Cell {
						position: Position { row: 0, col: 3 },
						content: [
							150, 6, 83, 12, 56, 17, 0, 225, 186, 238, 151, 181, 116, 1, 34, 240,
							174, 192, 98, 201, 60, 208, 50, 215, 90, 231, 2, 27, 17, 204, 140, 30,
							213, 253, 200, 176, 72, 98, 121, 25, 239, 76, 230, 154, 121, 246, 142,
							37, 85, 184, 201, 218, 107, 88, 0, 87, 199, 169, 98, 172, 4, 140, 151,
							65, 162, 162, 190, 205, 20, 95, 67, 114, 73, 59, 170, 52, 243, 140,
							237, 0,
						],
					},
				];
				Box::pin(async move { (fetch, dht) })
			});
		if cfg.disable_rpc {
			mock_client.expect_get_kate_proof().never();
		}
		mock_client
			.expect_is_confidence_in_db()
			.with(eq(42))
			.returning(|_| Ok(true));

		mock_client
			.expect_insert_cells_into_dht()
			.withf(move |x, _| *x == 42)
			.returning(move |_, _| Box::pin(async move { 1f32 }));
		process_block(&mock_client, 42, &cfg, pp, Some(block_tx))
			.await
			.unwrap();
	}

	#[tokio::test]
	pub async fn test_process_blocks_with_rpc() {
		let (block_tx, _) = channel::<types::BlockVerified>(10);
		let pp = Arc::new(testnet::public_params(1024));
		let cfg = SyncClientConfig::from(&RuntimeConfig::default());
		let mut mock_client = MockSyncClient::new();
		let header: DaHeader = DaHeader {
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
				app_lookup: CompactDataLookup {
					size: 1,
					index: vec![],
				},
			}),
		};
		let header_hash: H256 =
			hex!("3767f8955d6f7306b1e55701b6316fa1163daa8d4cffdb05c3b25db5f5da1723").into();
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
		mock_client
			.expect_block_header_in_db()
			.with(eq(42))
			.returning(|_| Ok(false));

		mock_client
			.expect_get_header_by_block_number()
			.with(eq(42))
			.returning(move |_| {
				let header = header.clone();

				Box::pin(async move { Ok((header, header_hash)) })
			});
		mock_client
			.expect_store_block_header_in_db()
			.withf(|_, x| *x == 42)
			.returning(|_, _| Ok(()));
		mock_client
			.expect_is_confidence_in_db()
			.with(eq(42))
			.returning(|_| Ok(true));
		mock_client
			.expect_fetch_cells_from_dht()
			.withf(|_, x: &u32| *x == 42)
			.returning(|_, _| {
				let dht = vec![Position { row: 0, col: 3 }];
				let fetch: Vec<Cell> = vec![
					Cell {
						position: Position { row: 0, col: 0 },
						content: [
							183, 56, 112, 134, 157, 186, 15, 255, 245, 173, 188, 37, 165, 224, 226,
							80, 196, 137, 235, 233, 154, 4, 110, 142, 26, 95, 150, 132, 61, 23,
							202, 212, 101, 6, 235, 6, 102, 188, 206, 147, 36, 121, 128, 63, 240,
							37, 200, 236, 4, 44, 40, 4, 3, 0, 11, 35, 249, 222, 81, 135, 1, 128, 0,
							0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
						],
					},
					Cell {
						position: Position { row: 0, col: 2 },
						content: [
							153, 31, 34, 70, 221, 239, 97, 236, 3, 172, 44, 167, 114, 117, 186,
							245, 171, 12, 70, 144, 204, 207, 82, 160, 29, 83, 245, 203, 40, 238,
							96, 131, 68, 96, 9, 136, 151, 88, 218, 72, 79, 55, 193, 228, 71, 193,
							120, 113, 48, 237, 151, 135, 246, 8, 251, 150, 106, 44, 29, 250, 250,
							54, 133, 203, 162, 73, 252, 32, 42, 175, 24, 166, 142, 72, 226, 150,
							163, 206, 115, 0,
						],
					},
					Cell {
						position: Position { row: 1, col: 1 },
						content: [
							146, 211, 61, 65, 166, 68, 252, 65, 196, 167, 211, 64, 223, 151, 33,
							133, 67, 132, 59, 13, 224, 100, 55, 104, 180, 174, 17, 41, 151, 125,
							193, 80, 142, 140, 216, 97, 117, 60, 217, 44, 242, 7, 30, 204, 22, 197,
							12, 179, 88, 163, 102, 4, 54, 208, 14, 161, 193, 25, 34, 179, 35, 234,
							120, 131, 62, 53, 0, 54, 72, 49, 196, 234, 239, 65, 25, 159, 245, 38,
							193, 0,
						],
					},
				];
				Box::pin(async move { (fetch, dht) })
			});
		if cfg.disable_rpc {
			mock_client.expect_get_kate_proof().never();
		} else {
			mock_client.expect_get_kate_proof().returning(move |_, _| {
				let unfetched = unfetched.clone();
				Box::pin(async move { Ok(unfetched) })
			});
		}
		mock_client
			.expect_is_confidence_in_db()
			.with(eq(42))
			.returning(|_| Ok(true));
		mock_client
			.expect_insert_cells_into_dht()
			.withf(move |x, _| *x == 42)
			.returning(move |_, _| Box::pin(async move { 1f32 }));
		process_block(&mock_client, 42, &cfg, pp, Some(block_tx))
			.await
			.unwrap();
	}
	#[tokio::test]
	pub async fn test_header_in_dbstore() {
		let (block_tx, _) = channel::<types::BlockVerified>(10);
		let pp = Arc::new(testnet::public_params(1024));
		let cfg = SyncClientConfig::from(&RuntimeConfig::default());
		let mut mock_client = MockSyncClient::new();
		mock_client
			.expect_block_header_in_db()
			.withf(|block: &u32| *block == 42)
			.returning(|_| Ok(true));
		mock_client.expect_get_header_by_block_number().never();
		mock_client.block_header_in_db(42).unwrap();
		process_block(&mock_client, 42, &cfg, pp, Some(block_tx))
			.await
			.unwrap();
	}
}
