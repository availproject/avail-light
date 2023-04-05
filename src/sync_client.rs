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
use std::{sync::Arc, time::SystemTime};
use subxt::{utils::H256, OnlineClient};
// use avail_subxt::primitives::Header as DaHeader;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use futures::stream::{self, StreamExt};
use kate_recovery::{commitments, matrix::Dimensions};
use rocksdb::DB;
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
	fn store_confidence_in_db(&self, count: u32, block_number: u32) -> Result<()>;
	fn cell_count_for_confidence(&self, cfg: &SyncClientConfig) -> Result<u32>;
	fn generate_random_cells(
		&self,
		header: DaHeader,
		cfg: &SyncClientConfig,
	) -> Result<Vec<Position>>;
	async fn fetch_cells_rpc(
		&self,
		cfg: &SyncClientConfig,
		header_hash: H256,
		unfetched: &Vec<Position>,
	) -> Result<Vec<Cell>>;
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>>;
	async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> Result<f32>;
	async fn fetch_cells_from_dht(
		&self,
		positions: &[Position],
		block_number: u32,
	) -> Result<(Vec<Cell>, Vec<Position>)>;
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

		info!("Header {:?}", header);
		info!("\nHash {:?}", header_hash);
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

		// now this is in `u64`
		// let cell_count = test.cell_count_for_confidence()?;
		let positions = self
			.generate_random_cells(header.clone(), cfg)
			.context("Failed to generate random cells")?;

		let (dht_fetched, unfetched) = self.fetch_cells_from_dht(&positions, block_number).await?;
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

		let (verified, _) =
			self.verify_cells(block_number, &dimensions, &cells, &commitments, &pp)?;

		info!(
			block_number,
			"Completed {cells_len} verification rounds: \t{:?}",
			begin.elapsed()?
		);

		// write confidence factor into on-disk database
		self.store_confidence_in_db(verified.len().try_into()?, block_number)
			.context("Failed to store confidence in DB")?;

		let inserted_cells = self
			.insert_cells_into_dht(block_number, rpc_fetched)
			.await?;
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

// trait SyncClone {
//     fn clone_box(&self) -> Box<dyn SyncClient>;
// }

// impl<T> SyncClone for T
// where
//     T: 'static + SyncClient + Clone,
// {
//     fn clone_box(&self) -> Box<dyn SyncClient> {
//         Box::new(self.clone())
//     }
// }

#[derive(Clone)]
pub struct SyncClientImpl {
	// cfg: SyncClientConfig,
	db: Arc<DB>,
	// pp: PublicParameters,
	// block_tx: Option<Sender<BlockVerified>>,
	// block_number: u32,
	network_client: Client,
	rpc_client: OnlineClient<AvailConfig>,
}

#[async_trait]
impl SyncClient for SyncClientImpl {
	fn block_header_in_db(&self, block_number: u32) -> Result<bool> {
		Ok(is_block_header_in_db(self.db.clone(), block_number)
			.context("Failed to check if block header is in DB")?)
	}

	async fn get_header_by_block_number(&self, block_number: u32) -> Result<(DaHeader, H256)> {
		Ok(
			rpc::get_header_by_block_number(&self.rpc_client, block_number)
				.await
				.context("Failed to get block {block_number} by block number")?,
		)
	}

	fn store_block_header_in_db(&self, header: DaHeader, block_number: u32) -> Result<()> {
		Ok(
			store_block_header_in_db(self.db.clone(), block_number, &header)
				.context("Failed to store block header in DB")?,
		)
	}

	fn is_confidence_in_db(&self, block_number: u32) -> Result<bool> {
		Ok(is_confidence_in_db(self.db.clone(), block_number)
			.context("Failed to check if confidence is in DB")?)
	}

	fn store_confidence_in_db(&self, count: u32, block_number: u32) -> Result<()> {
		Ok(store_confidence_in_db(self.db.clone(), block_number, count)
			.context("Failed to store confidence in DB")?)
	}

	fn cell_count_for_confidence(&self, cfg: &SyncClientConfig) -> Result<u32> {
		let conf = rpc::cell_count_for_confidence(cfg.confidence);
		Ok(conf)
	}

	fn generate_random_cells(
		&self,
		header: DaHeader,
		cfg: &SyncClientConfig,
	) -> Result<Vec<Position>> {
		let conf = self.cell_count_for_confidence(cfg)?;
		let HeaderExtension::V1(xt) = header.extension.clone();
		let dimensions = Dimensions::new(xt.commitment.rows, xt.commitment.cols).unwrap();
		let cells = rpc::generate_random_cells(&dimensions, conf);
		Ok(cells)
	}

	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>> {
		Ok(rpc::get_kate_proof(&self.rpc_client, hash, positions).await?)
	}

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

	async fn fetch_cells_rpc(
		&self,
		cfg: &SyncClientConfig,
		header_hash: H256,
		unfetched: &Vec<Position>,
	) -> Result<Vec<Cell>> {
		let fetched = if cfg.disable_rpc {
			vec![]
		} else {
			self.get_kate_proof(header_hash, unfetched).await?
		};
		Ok(fetched)
	}
	// async fn process_block(
	// 	&self,
	// 	block_number: u32,
	// 	cfg: &SyncClientConfig,
	// 	pp: PublicParameters,
	// 	block_tx: Option<Sender<BlockVerified>>,
	// ) -> Result<()> {
	// 	Ok(process_block(&self, block_number, cfg, pp, block_tx).await?)
	// }
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
					// cfg: cfg_clone.clone(),
					db: store.clone(),
					// pp: pp.clone(),
					// block_tx: block_tx.clone(),
					// block_number,
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
	use subxt::config::substrate::DigestItem::{PreRuntime, Seal};
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
		// process_blocks(mock_client, 42, &cfg, pp, Some(block_tx)).await.unwrap();
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
		// mock_expect_get_kate_pr = testnet::public_params(1024);
		let cfg = SyncClientConfig::from(&RuntimeConfig::default());
		let mut mock_client = MockSyncClient::new();
		if cfg.disable_rpc == true {
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
		// let mut seq = Sequence::new();
		let (block_tx, _) = channel::<types::BlockVerified>(10);
		let pp = testnet::public_params(1024);
		// mock_expect_get_kate_pr = testnet::public_params(1024);
		let cfg = SyncClientConfig::from(&RuntimeConfig::default());
		let mut mock_client = MockSyncClient::new();
		mock_client.expect_block_header_in_db().never();
		// .with(eq(42));
		// .returning(move |_| {
		// 	let x = x.clone();
		// 	Box::pin(async move {
		// 		Ok(x)
		// 	})
		// });

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
		let header_hash: H256 =
			hex!("3767f8955d6f7306b1e55701b6316fa1163daa8d4cffdb05c3b25db5f5da1723").into();
		mock_client
			.expect_get_header_by_block_number()
			.with(eq(2))
			.returning(move |_| {
				let header = header.clone();
				let header_hash = header_hash.clone();
				// let x = x.clone();

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
				Box::pin(async move { Ok((fetch, dht)) })
			});
		// mock_client.get_header_by_block_number(42).await.unwrap();

		mock_client
			.expect_process_block()
			.returning(|_, _, _, _| Box::pin(async move { Ok(()) }));
		mock_client
			.process_block(42, &cfg, pp, Some(block_tx))
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
			// .with(|_, h:H256, _| h == header_hash)
			.returning(move |_, _, _| {
				let unfetched = unfetched.clone();
				Box::pin(async move { Ok(unfetched) })
			});
		mock_client
			.fetch_cells_rpc(&cfg, header_hash, &dht)
			.await
			.unwrap();
	}
}
