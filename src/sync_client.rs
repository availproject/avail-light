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
	async fn fetch_cells_rpc(&self, cfg: &SyncClientConfig, header_hash: H256, unfetched: &Vec<Position>) -> Result<Vec<Cell>>;
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
	) -> Result<()>{
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

	let dimensions =
		Dimensions::new(xt.commitment.rows, xt.commitment.cols).context("Invalid dimensions")?;

	let commitments = commitments::from_slice(&xt.commitment.commitment)?;

	// now this is in `u64`
	// let cell_count = test.cell_count_for_confidence()?;
	let positions = self
		.generate_random_cells(header.clone(), cfg)
		.context("Failed to generate random cells")?;

	let (dht_fetched, unfetched) = self
		.fetch_cells_from_dht(&positions, block_number)
		.await?;

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

	let (verified, _) = self.verify_cells(block_number, &dimensions, &cells, &commitments, &pp)?;

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

	async fn fetch_cells_rpc(&self, cfg: &SyncClientConfig, header_hash:H256, unfetched: &Vec<Position>) -> Result<Vec<Cell>>{
		let fetched = if cfg.disable_rpc{
			vec![]
		}else{
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
		let proof = proof::verify(
			block_num,
			dimensions,
			cells,
			commitments,
			public_parameters,
		)?;
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
	use kate_recovery::testnet;
	use mockall::{predicate::eq, Sequence};
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
	async fn test_kate_proof(){
		let mut seq = Sequence::new();
		let (block_tx, _) = channel::<types::BlockVerified>(10);
		let pp = testnet::public_params(1024);
		// mock_expect_get_kate_pr = testnet::public_params(1024);
		let cfg = SyncClientConfig::from(&RuntimeConfig::default());
		let mut mock_client = MockSyncClient::new();
		if cfg.disable_rpc == true{ 
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
		}else{
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
}
