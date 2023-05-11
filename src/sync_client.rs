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

use anyhow::{anyhow, Context, Ok, Result};
use async_trait::async_trait;
use avail_subxt::{
	api::runtime_types::da_primitives::header::extension::HeaderExtension,
	primitives::Header as DaHeader, AvailConfig,
};
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use futures::stream::{self, StreamExt};
use kate_recovery::{commitments, matrix::Dimensions};
use rocksdb::DB;
use std::{sync::Arc, time::SystemTime};
use subxt::{utils::H256, OnlineClient};
use tokio::sync::mpsc::Sender;
use tracing::{error, info, warn};

use crate::{
	data::{
		is_block_header_in_db, is_confidence_in_db, store_block_header_in_db,
		store_confidence_in_db,
	},
	network::Client,
	proof, rpc,
	types::{BlockVerified, SyncClientConfig},
};
use kate_recovery::{data::Cell, matrix::Position};
use mockall::automock;
#[async_trait]
#[automock]
pub trait SyncClient {
	fn block_header_in_db(&self, block_number: u32) -> Result<bool>;
	async fn get_header_by_block_number(&self, block_number: u32) -> Result<(DaHeader, H256)>;
	fn store_block_header_in_db(&self, header: DaHeader, block_number: u32) -> Result<()>;
	fn is_confidence_in_db(&self, block_number: u32) -> Result<bool>;
	fn store_confidence_in_db(&self, count: u32, block_number: u32) -> Result<bool>;
	async fn fetch_cells_rpc(
		&self,
		cfg: &SyncClientConfig,
		header_hash: H256,
		unfetched: &[Position],
	) -> Result<Vec<Cell>>;
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>>;
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> f32;
	async fn fetch_cells_from_dht(
		&self,
		positions: &[Position],
		block_number: u32,
	) -> (Vec<Cell>, Vec<Position>);
	async fn process_block(
		&self,
		block_number: u32,
		cfg: &SyncClientConfig,
		pp: PublicParameters,
		block_tx: Option<Sender<BlockVerified>>,
	) -> Result<()> {
		if self
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
		let begin = SystemTime::now();

		let (header, header_hash) = self
			.get_header_by_block_number(block_number)
			.await
			.context("Failed to get block {block_number} by block number")?;

		let HeaderExtension::V1(xt) = &header.extension;

		info!(block_number, "App index {:?}", xt.app_lookup.index);

		self.store_block_header_in_db(header.clone(), block_number)
			.context("Failed to store block header in DB")?;

		info!(
			block_number,
			"Synced block header: \t{:?}",
			begin.elapsed()?
		);

		// If it's found that this certain block is not verified
		// then it'll be verified now
		if self
			.is_confidence_in_db(block_number)
			.context("Failed to check if confidence is in DB")?
		{
			return Ok(());
		};

		let begin = SystemTime::now();

		let dimensions = Dimensions::new(xt.commitment.rows, xt.commitment.cols)
			.context("Invalid dimensions")?;

		let commitments = commitments::from_slice(&xt.commitment.commitment)?;

		let cell_count = rpc::cell_count_for_confidence(cfg.confidence);
		let positions = rpc::generate_random_cells(&dimensions, cell_count);

		let (dht_fetched, unfetched) = self.fetch_cells_from_dht(&positions, block_number).await;
		info!("dht fetch {:?}, unfetch {:?}", dht_fetched, unfetched);

		info!(
			block_number,
			"Number of cells fetched from DHT: {}",
			dht_fetched.len()
		);

		let rpc_fetched = self.fetch_cells_rpc(cfg, header_hash, &unfetched).await?;

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

		let (verified, unverified) =
			self.verify_cells(block_number, &dimensions, &cells, &commitments, &pp)?;

		let ver = (verified.clone(), unverified);
		info!("verified {:?}", ver);

		info!(
			block_number,
			"Completed {cells_len} verification rounds: \t{:?}",
			begin.elapsed()?
		);

		self.store_confidence_in_db(verified.len().try_into()?, block_number)
			.context("Failed to store confidence in DB")?;

		let inserted_cells = self.insert_cells_into_dht(block_number, rpc_fetched).await;
		info!(block_number, "Cells inserted into DHT: {inserted_cells}");

		let client_msg = BlockVerified::try_from(header).context("converting to message failed")?;

		if let Some(ref channel) = block_tx {
			if let Err(error) = channel.send(client_msg).await {
				error!("Cannot send block verified message: {error}");
			}
		}

		Ok(())
	}
	fn verify_cells(
		&self,
		block_num: u32,
		dimensions: &Dimensions,
		cells: &[Cell],
		commitments: &[[u8; 48]],
		public_parameters: &PublicParameters,
	) -> Result<(Vec<Position>, Vec<Position>)>;
}

#[derive(Clone)]
pub struct SyncClientImpl {
	db: Arc<DB>,
	network_client: Client,
	rpc_client: OnlineClient<AvailConfig>,
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
			.context("Failed to get block {block_number} by block number")
	}

	fn store_block_header_in_db(&self, header: DaHeader, block_number: u32) -> Result<()> {
		store_block_header_in_db(self.db.clone(), block_number, &header)
			.context("Failed to store block header in DB")
	}

	fn is_confidence_in_db(&self, block_number: u32) -> Result<bool> {
		is_confidence_in_db(self.db.clone(), block_number)
			.context("Failed to check if confidence is in DB")
	}

	fn store_confidence_in_db(&self, count: u32, block_number: u32) -> Result<bool> {
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

	async fn fetch_cells_rpc(
		&self,
		cfg: &SyncClientConfig,
		header_hash: H256,
		unfetched: &[Position],
	) -> Result<Vec<Cell>> {
		if cfg.disable_rpc {
			Ok(vec![])
		} else {
			self.get_kate_proof(header_hash, unfetched).await
		}
	}

	fn verify_cells(
		&self,
		block_num: u32,
		dimensions: &Dimensions,
		cells: &[Cell],
		commitments: &[[u8; 48]],
		public_parameters: &PublicParameters,
	) -> Result<(Vec<Position>, Vec<Position>)> {
		proof::verify(block_num, dimensions, cells, commitments, public_parameters)
			.map_err(From::from)
	}
}

/// Runs sync client.
///
/// # Arguments
///
/// * `cfg` - sync client configuration
/// * `rpc_client` - Node's RPC subxt client for fetching data unavailable in DHT (if configured)
/// * `end_block` - Latest block to sync
/// * `sync_blocks_depth` - How many blocks in past to sync
/// * `db` - Database to store confidence and block header
/// * `network_client` - Reference to a libp2p custom network client
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
pub async fn run(
	cfg: SyncClientConfig,
	rpc_client: OnlineClient<AvailConfig>,
	end_block: u32,
	sync_blocks_depth: u32,
	db: Arc<DB>,
	network_client: Client,
	pp: PublicParameters,
	block_tx: Option<Sender<BlockVerified>>,
) {
	if sync_blocks_depth >= 250 {
		warn!("In order to process {sync_blocks_depth} blocks behind latest block, connected nodes needs to be archive nodes!");
	}
	let start_block = end_block.saturating_sub(sync_blocks_depth);
	info!("Syncing block headers from {start_block} to {end_block}");
	let blocks = (start_block..=end_block).map(move |b| {
		(
			b,
			rpc_client.clone(),
			db.clone(),
			network_client.clone(),
			pp.clone(),
			block_tx.clone(),
		)
	});
	let cfg_clone = &cfg;
	stream::iter(blocks)
		.for_each_concurrent(
			num_cpus::get(), // number of logical CPUs available on machine
			// run those many concurrent syncing lightweight tasks, not threads
			|(block_number, rpc_client, store, net_svc, pp, block_tx)| async move {
				let test = SyncClientImpl {
					db: store.clone(),
					network_client: net_svc.clone(),
					rpc_client: rpc_client.clone(),
				};
				// TODO: Should we handle unprocessed blocks differently?
				if let Err(error) = test
					.process_block(block_number, cfg_clone, pp, block_tx)
					.await
				{
					error!(block_number, "Cannot process block: {error:#}");
				}
			},
		)
		.await;
}

#[cfg(test)]
mod tests {

	use super::*;
	use crate::types::{self, RuntimeConfig};
	use avail_subxt::api::runtime_types::da_primitives::asdr::data_lookup::DataLookup;
	use avail_subxt::api::runtime_types::da_primitives::header::extension::v1::HeaderExtension;
	use avail_subxt::api::runtime_types::da_primitives::header::extension::HeaderExtension::V1;
	use avail_subxt::api::runtime_types::da_primitives::kate_commitment::KateCommitment;
	use hex_literal::hex;
	use kate_recovery::testnet;
	use mockall::{predicate::eq, Sequence};
	use subxt::config::substrate::Digest;
	use tokio::sync::mpsc::channel;

	#[tokio::test]
	pub async fn test_process_blocks() {
		let (block_tx, _) = channel::<types::BlockVerified>(10);
		let pp = testnet::public_params(1024);
		let cfg = SyncClientConfig::from(&RuntimeConfig::default());
		let mut mock_client = MockSyncClient::new();
		mock_client
			.expect_process_block()
			.returning(|_, _, _, _| Box::pin(async move { Ok(()) }));
		mock_client
			.process_block(42, &cfg, pp, Some(block_tx))
			.await
			.unwrap();
	}

	#[tokio::test]
	pub async fn test_db_in_store() {
		let mut seq = Sequence::new();
		let (block_tx, _) = channel::<types::BlockVerified>(10);
		let pp = testnet::public_params(1024);
		let cfg = SyncClientConfig::from(&RuntimeConfig::default());
		let mut mock_client = MockSyncClient::new();
		mock_client
			.expect_block_header_in_db()
			.withf(|block: &u32| *block == 42)
			.times(1)
			.in_sequence(&mut seq)
			.returning(|_| Ok(true));
		mock_client.expect_get_header_by_block_number().never();
		mock_client.block_header_in_db(42).unwrap();
		mock_client
			.expect_process_block()
			.times(1)
			.in_sequence(&mut seq)
			.returning(|_, _, _, _| Box::pin(async move { Ok(()) }));
		mock_client
			.process_block(42, &cfg, pp, Some(block_tx))
			.await
			.unwrap();
	}
	#[tokio::test]
	async fn test_confidence_in_db() {
		let mut seq = Sequence::new();
		let (block_tx, _) = channel::<types::BlockVerified>(10);
		let pp = testnet::public_params(1024);
		let cfg = SyncClientConfig::from(&RuntimeConfig::default());
		let mut mock_client = MockSyncClient::new();
		mock_client
			.expect_is_confidence_in_db()
			.with(eq(42))
			.times(1)
			.in_sequence(&mut seq)
			.returning(|_| Ok(true));
		mock_client.expect_verify_cells().never();
		mock_client.is_confidence_in_db(42).unwrap();
		mock_client
			.expect_process_block()
			.withf(|x: &u32, _, _, _| *x == 42)
			.times(1)
			.in_sequence(&mut seq)
			.returning(|_, _, _, _| Box::pin(async move { Ok(()) }));
		mock_client
			.process_block(42, &cfg, pp, Some(block_tx))
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn test_kate_proof() {
		let mut seq = Sequence::new();
		let (block_tx, _) = channel::<types::BlockVerified>(10);
		let pp = testnet::public_params(1024);
		let cfg = SyncClientConfig::from(&RuntimeConfig::default());
		let mut mock_client = MockSyncClient::new();
		if cfg.disable_rpc {
			mock_client.expect_fetch_cells_rpc().never();
			mock_client
				.expect_process_block()
				.times(1)
				.in_sequence(&mut seq)
				.returning(|_, _, _, _| Box::pin(async move { Ok(()) }));
			mock_client
				.process_block(42, &cfg, pp, Some(block_tx))
				.await
				.unwrap();
		} else {
			mock_client
				.expect_process_block()
				.times(1)
				.in_sequence(&mut seq)
				.returning(|_, _, _, _| Box::pin(async move { Ok(()) }));
			mock_client
				.process_block(42, &cfg, pp, Some(block_tx))
				.await
				.unwrap();
		}
	}

	//TODO: Code Cleaning/ formating
	#[tokio::test]
	async fn test_fetch_cells_dht() {
		let (block_tx, _) = channel::<types::BlockVerified>(10);
		let pp = testnet::public_params(1024);
		let cfg = SyncClientConfig::from(&RuntimeConfig::default());
		let mut mock_client = MockSyncClient::new();
		mock_client.expect_block_header_in_db().never();

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
				app_lookup: DataLookup {
					size: 1,
					index: vec![],
				},
			}),
		};
		let header_hash: H256 =
			hex!("3767f8955d6f7306b1e55701b6316fa1163daa8d4cffdb05c3b25db5f5da1723").into();
		mock_client
			.expect_get_header_by_block_number()
			.with(eq(2))
			.returning(move |_| {
				let header = header.clone();
				let header_hash = header_hash;
				Box::pin(async move { Ok((header, header_hash)) })
			});
		mock_client
			.expect_fetch_cells_from_dht()
			.withf(|_, x: &u32| *x == 2)
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

		let rpc_fetched = vec![Cell {
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
			.expect_insert_cells_into_dht()
			.withf(move |x, _| *x == 2)
			.returning(move |_, _| Box::pin(async move { 1f32 }));
		mock_client.insert_cells_into_dht(2, rpc_fetched).await;
		mock_client
			.expect_process_block()
			.returning(|_, _, _, _| Box::pin(async move { Ok(()) }));
		mock_client
			.process_block(2, &cfg, pp, Some(block_tx))
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn test_fetch_rpc() {
		let cfg = SyncClientConfig::from(&RuntimeConfig::default());
		let mut mock_client = MockSyncClient::new();
		let dht = vec![Position { row: 0, col: 3 }];
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
		let header_hash: H256 =
			hex!("3767f8955d6f7306b1e55701b6316fa1163daa8d4cffdb05c3b25db5f5da1723").into();
		mock_client.expect_block_header_in_db().never();
		mock_client
			.expect_fetch_cells_rpc()
			.returning(move |_, _, _| {
				let unfetched = unfetched.clone();
				Box::pin(async move { Ok(unfetched) })
			});
		mock_client
			.fetch_cells_rpc(&cfg, header_hash, &dht)
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn test_verify_cells() {
		let (block_tx, _) = channel::<types::BlockVerified>(10);
		let pp = testnet::public_params(1024);
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
		mock_client
			.expect_process_block()
			.returning(|_, _, _, _| Box::pin(async move { Ok(()) }));
		mock_client
			.process_block(2, &cfg, pp, Some(block_tx))
			.await
			.unwrap();
	}
}
