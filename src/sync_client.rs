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

use anyhow::{anyhow, Context, Result};
use avail_subxt::api::runtime_types::da_primitives::header::extension::HeaderExtension;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use futures::stream::{self, StreamExt};
use kate_recovery::matrix::Dimensions;
use rocksdb::DB;
use tracing::{error, info, warn};

use crate::{
	data::{
		fetch_cells_from_dht, insert_into_dht, is_block_header_in_db, is_confidence_in_db,
		store_block_header_in_db, store_confidence_in_db,
	},
	network::Client,
	rpc,
	types::SyncClientConfig,
};

async fn process_block(
	cfg: &SyncClientConfig,
	rpc_url: String,
	db: Arc<DB>,
	block_number: u32,
	network_client: Client,
	pp: PublicParameters,
) -> Result<()> {
	if is_block_header_in_db(db.clone(), block_number)
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

	let (header, header_hash) = rpc::get_header_by_block_number(&rpc_url, block_number)
		.await
		.context("Failed to get block {block_number} by block number")?;

	let HeaderExtension::V1(xt) = &header.extension;

	info!(block_number, "App index {:?}", xt.app_lookup.index);

	store_block_header_in_db(db.clone(), block_number, &header)
		.context("Failed to store block header in DB")?;

	info!(
		block_number,
		"Synced block header: \t{:?}",
		begin.elapsed()?
	);

	// If it's found that this certain block is not verified
	// then it'll be verified now
	if is_confidence_in_db(db.clone(), block_number)
		.context("Failed to check if confidence is in DB")?
	{
		return Ok(());
	};

	let begin = SystemTime::now();

	let dimensions =
		Dimensions::new(xt.commitment.rows, xt.commitment.cols).context("Invalid dimensions")?;
	let commitment = xt.commitment.commitment.clone();
	// now this is in `u64`
	let cell_count = rpc::cell_count_for_confidence(cfg.confidence);
	let positions = rpc::generate_random_cells(&dimensions, cell_count);

	let (dht_fetched, unfetched) = fetch_cells_from_dht(
		&network_client,
		block_number,
		&positions,
		cfg.dht_parallelization_limit,
	)
	.await
	.context("Failed to fetch cells from DHT")?;

	info!(
		block_number,
		"Number of cells fetched from DHT: {}",
		dht_fetched.len()
	);

	let rpc_fetched = if cfg.disable_rpc {
		vec![]
	} else {
		rpc::get_kate_proof(&rpc_url, header_hash, unfetched)
			.await
			.context("Failed to fetch cells from node RPC")?
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

	info!(
		block_number,
		"Fetched {} cells for verification",
		cells.len()
	);

	let count = crate::proof::verify_proof(block_number, &dimensions, &cells, commitment, pp);

	info!(
		block_number,
		"Completed {count} verification rounds: \t{:?}",
		begin.elapsed()?
	);
	// write confidence factor into on-disk database
	store_confidence_in_db(db.clone(), block_number, count as u32)
		.context("Failed to store confidence in DB")?;

	insert_into_dht(
		&network_client,
		block_number,
		rpc_fetched,
		cfg.dht_parallelization_limit,
		cfg.ttl,
	)
	.await;
	info!(block_number, "Cells inserted into DHT");
	Ok(())
}

/// Runs sync client.
///
/// # Arguments
///
/// * `cfg` - sync client configuration
/// * `rpc_url` - Node's RPC URL for fetching data unavailable in DHT (if configured)
/// * `end_block` - Latest block to sync
/// * `sync_blocks_depth` - How many blocks in past to sync
/// * `db` - Database to store confidence and block header
/// * `network_client` - Reference to a libp2p custom network client
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
pub async fn run(
	cfg: SyncClientConfig,
	rpc_url: String,
	end_block: u32,
	sync_blocks_depth: u32,
	db: Arc<DB>,
	network_client: Client,
	pp: PublicParameters,
) {
	if sync_blocks_depth >= 250 {
		warn!("In order to process {sync_blocks_depth} blocks behind latest block, connected nodes needs to be archive nodes!");
	}
	let start_block = end_block.saturating_sub(sync_blocks_depth);
	info!("Syncing block headers from {start_block} to {end_block}");
	let blocks = (start_block..=end_block).map(move |b| {
		(
			b,
			rpc_url.clone(),
			db.clone(),
			network_client.clone(),
			pp.clone(),
		)
	});
	let cfg_clone = &cfg;
	stream::iter(blocks)
		.for_each_concurrent(
			num_cpus::get(), // number of logical CPUs available on machine
			// run those many concurrent syncing lightweight tasks, not threads
			|(block_number, rpc_url, store, net_svc, pp)| async move {
				// TODO: Should we handle unprocessed blocks differently?
				if let Err(error) =
					process_block(cfg_clone, rpc_url, store, block_number, net_svc, pp.clone())
						.await
				{
					error!(block_number, "Cannot process block: {error:#}");
				}
			},
		)
		.await;
}
