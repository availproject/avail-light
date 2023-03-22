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

use std::{sync::Arc, time::SystemTime};
use async_trait::async_trait;
use anyhow::{anyhow, Context, Result, Ok};
use avail_subxt::{
	api::runtime_types::da_primitives::header::extension::HeaderExtension, AvailConfig, primitives::Header as DaHeader,
};
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
use kate_recovery::{
	data::Cell,
	matrix::{Position},
};
use mockall::automock;
#[async_trait]
#[automock]
pub trait SyncClient{
	fn block_header_in_db(&self) -> Result<bool>;
	async fn get_header_by_block_number(&self) -> Result<(DaHeader, H256)>;
	fn store_block_header_in_db(&self, header: DaHeader) -> Result<()>;
	fn is_confidence_in_db(&self) -> Result<bool>;
	fn store_confidence_in_db(&self, count:u32) -> Result<()>;
	fn cell_count_for_confidence(&self) -> Result<u32>;
	fn generate_random_cells(&self, header:DaHeader) -> Result<Vec<Position>>;
	async fn get_kate_proof(&self, hash: H256, positions: &[Position]) -> Result<Vec<Cell>>;
	async fn insert_cells_into_dht(&self,block:u32, cells: Vec<Cell>) -> Result<f32>;
	async fn fetch_cells_from_dht(&self, positions: &[Position]) -> Result<(Vec<Cell>, Vec<Position>)>;
	async fn process_block(&self) -> Result<()>;
}

pub struct SyncClientImpl {
	cfg: SyncClientConfig,
	db: Arc<DB>,
	pp: PublicParameters,
	block_tx: Option<Sender<BlockVerified>>,
	block_number: u32,
	network_client: Client,
	rpc_client: OnlineClient<AvailConfig>,
}

#[async_trait]
impl SyncClient for SyncClientImpl{
	fn block_header_in_db(&self) -> Result<bool> {
		Ok(is_block_header_in_db(self.db.clone(), self.block_number)
			.context("Failed to check if block header is in DB")?)
	}

	async fn get_header_by_block_number(&self) -> Result<(DaHeader,H256)> {
		Ok(rpc::get_header_by_block_number(&self.rpc_client, self.block_number).await
			.context("Failed to get block {block_number} by block number")?)
	}

	fn store_block_header_in_db(&self, header: DaHeader) -> Result<()> {
		Ok(store_block_header_in_db(self.db.clone(), self.block_number, &header)
			.context("Failed to store block header in DB")?)
	}

	fn is_confidence_in_db(&self) -> Result<bool> {
		Ok(is_confidence_in_db(self.db.clone(), self.block_number)
			.context("Failed to check if confidence is in DB")?)
	}

	fn store_confidence_in_db(&self,count:u32) -> Result<()> {
		Ok(store_confidence_in_db(self.db.clone(), self.block_number, count)
			.context("Failed to store confidence in DB")?)
	}

	fn cell_count_for_confidence(&self) -> Result<u32> {
		let conf = rpc::cell_count_for_confidence(self.cfg.confidence);
		Ok(conf)
	}

	fn generate_random_cells(&self, header:DaHeader) -> Result<Vec<Position>> {
		let conf = self.cell_count_for_confidence()?;
		let HeaderExtension::V1(xt) = header.extension.clone();
		let dimensions = Dimensions::new(xt.commitment.rows,xt.commitment.cols).unwrap();
		let cells = rpc::generate_random_cells(&dimensions,conf);
		Ok(cells)
	}

	async fn get_kate_proof(&self,hash:H256,positions: &[Position]) -> Result<Vec<Cell>> {
		Ok(rpc::get_kate_proof(&self.rpc_client,hash,positions).await?)
	}

	async fn insert_cells_into_dht(&self,block:u32,cells: Vec<Cell>) -> Result<f32> {
		Ok(self.network_client.insert_cells_into_dht(block,cells).await)
	}
	async fn fetch_cells_from_dht(&self,positions: &[Position]) -> Result<(Vec<Cell>, Vec<Position>)> {
		Ok(self.network_client.fetch_cells_from_dht(self.block_number,positions).await)
	}
	async fn process_block(&self) -> Result<()>{
		Ok(process_block(&self).await?)
	}
		
}

pub async fn process_block(
	test: &SyncClientImpl,
) -> Result<()> {
	if test.block_header_in_db()
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

	let (header, header_hash) = test.get_header_by_block_number()
		.await
		.context("Failed to get block {block_number} by block number")?;

	let HeaderExtension::V1(xt) = &header.extension;

	info!(test.block_number, "App index {:?}", xt.app_lookup.index);

	test.store_block_header_in_db(header.clone())
		.context("Failed to store block header in DB")?;

	info!(
		test.block_number,
		"Synced block header: \t{:?}",
		begin.elapsed()?
	);

	// If it's found that this certain block is not verified
	// then it'll be verified now
	if test.is_confidence_in_db()
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
	let positions = test.generate_random_cells(header.clone())
		.context("Failed to generate random cells")?;

	let (dht_fetched, unfetched) = test.network_client
		.fetch_cells_from_dht(test.block_number, &positions)
		.await;

	info!(
		test.block_number,
		"Number of cells fetched from DHT: {}",
		dht_fetched.len()
	);

	let rpc_fetched = if test.cfg.disable_rpc {
		vec![]
	} else {
		test.get_kate_proof(header_hash, &unfetched)
			.await
			.context("Failed to fetch cells from node RPC")?
	};

	info!(
		test.block_number,
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
	info!(test.block_number, "Fetched {cells_len} cells for verification");

	let (verified, _) = proof::verify(test.block_number, &dimensions, &cells, &commitments, &test.pp)?;

	info!(
		test.block_number,
		"Completed {cells_len} verification rounds: \t{:?}",
		begin.elapsed()?
	);

	// write confidence factor into on-disk database
	test.store_confidence_in_db(verified.len().try_into()?)
		.context("Failed to store confidence in DB")?;

	test.network_client
		.insert_cells_into_dht(test.block_number, rpc_fetched)
		.await; 
	info!(test.block_number, "Cells inserted into DHT");

	let client_msg = BlockVerified::try_from(header).context("converting to message failed")?;

	if let Some(ref channel) = test.block_tx {
		if let Err(error) = channel.send(client_msg).await {
			error!("Cannot send block verified message: {error}");
		}
	}

	Ok(())
}

pub async fn process_blocks<T>( client: T) ->Result<()>
where T: SyncClient
{
	Ok(client.process_block().await?)
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
					cfg: cfg_clone.clone(),
					db: store.clone(),
					pp: pp.clone(),
					block_tx: block_tx.clone(),
					block_number,
					network_client: net_svc.clone(),
					rpc_client: rpc_client.clone(),
				};
				// TODO: Should we handle unprocessed blocks differently?
				if let Err(error) = process_blocks(test)
				.await
				{
					error!(block_number, "Cannot process block: {error:#}");
				}
			},
		)
		.await;
}


#[cfg(test)]
mod tests{

	use super::*;

	#[tokio::test]
	pub async fn test_db_from_client()
		{	
			let mut mock_client = MockSyncClient::new();
			mock_client.expect_process_block().returning(|| Ok(()));
			process_blocks(mock_client);

		}
	
}