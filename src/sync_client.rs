use std::{sync::Arc, time::SystemTime};

use anyhow::{anyhow, Context, Result};
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use futures::stream::{self, StreamExt};
use ipfs_embed::{DefaultParams, Ipfs};
use rocksdb::DB;
use tracing::{error, info};

use crate::{
	data::{
		fetch_cells_from_dht, insert_into_dht, is_block_header_in_db, is_confidence_in_db,
		store_block_header_in_db, store_confidence_in_db,
	},
	rpc,
};

async fn process_block(
	url: String,
	store: Arc<DB>,
	block_num: u64,
	ipfs: Ipfs<DefaultParams>,
	max_parallel_fetch_tasks: usize,
	pp: PublicParameters,
	confidence: f64,
) -> Result<()> {
	if is_block_header_in_db(store.clone(), block_num)
		.context("Failed to check if block header is in DB")?
	{
		return Ok(());
	};

	// if block header look up fails, only then comes here for
	// fetching and storing block header as part of (light weight)
	// syncing process
	let begin = SystemTime::now();

	let block_body = rpc::get_block_by_number(&url, block_num)
		.await
		.context("Failed to get block {block_num} by block number")?;

	info!(
		"Block {block_num} app index {:?}",
		block_body.header.app_data_lookup.index
	);

	store_block_header_in_db(store.clone(), block_num, &block_body.header)
		.context("Failed to store block header in DB")?;

	info!("Synced block header of {block_num}\t{:?}", begin.elapsed()?);

	// If it's found that this certain block is not verified
	// then it'll be verified now
	if is_confidence_in_db(store.clone(), block_num)
		.context("Failed to check if confidence is in DB")?
	{
		return Ok(());
	};

	let begin = SystemTime::now();

	// TODO: Setting max rows * 2 to match extended matrix dimensions
	let max_rows = block_body.header.extrinsics_root.rows * 2;
	let max_cols = block_body.header.extrinsics_root.cols;
	let commitment = block_body.header.extrinsics_root.commitment;
	let block_num = block_body.header.number;
	// now this is in `u64`
	let cell_count = rpc::cell_count_for_confidence(confidence);
	let positions = rpc::generate_random_cells(max_rows, max_cols, cell_count);

	let (ipfs_fetched, unfetched) =
		fetch_cells_from_dht(&ipfs, block_num, &positions, max_parallel_fetch_tasks)
			.await
			.context("Failed to fetch cells from DHT")?;

	info!(
		"Number of cells fetched from DHT for block {}: {}",
		block_num,
		ipfs_fetched.len()
	);

	let rpc_fetched = rpc::get_kate_proof(&url, block_num, unfetched)
		.await
		.context("Failed to fetch cells from node RPC")?;

	info!(
		"Number of cells fetched from RPC for block {}: {}",
		block_num,
		rpc_fetched.len()
	);

	let mut cells = vec![];
	cells.extend(ipfs_fetched);
	cells.extend(rpc_fetched.clone());
	if positions.len() > cells.len() {
		return Err(anyhow!(
			"Failed to fetch {} cells",
			positions.len() - cells.len()
		));
	}

	info!(
		"Fetched {} cells of block {} for verification",
		cells.len(),
		block_num
	);

	let count = crate::proof::verify_proof(block_num, max_rows, max_cols, &cells, commitment, pp);

	info!(
		"Completed {count} verification rounds for block {block_num}\t{:?}",
		begin.elapsed()?
	);
	// write confidence factor into on-disk database
	store_confidence_in_db(store.clone(), block_num, count as u32)
		.context("Failed to store confidence in DB")?;

	insert_into_dht(&ipfs, block_num, rpc_fetched, max_parallel_fetch_tasks).await;
	info!("Cells inserted into DHT for block {block_num}");
	Ok(())
}

pub async fn run(
	url: String,
	start_block: u64,
	end_block: u64,
	header_store: Arc<DB>,
	ipfs: Ipfs<DefaultParams>,
	max_parallel_fetch_tasks: usize,
	pp: PublicParameters,
	confidence: f64,
) {
	info!("Syncing block headers from 0 to {}", end_block);
	let blocks = (start_block..=end_block).map(move |b| {
		(
			b,
			url.clone(),
			header_store.clone(),
			ipfs.clone(),
			pp.clone(),
		)
	});
	stream::iter(blocks)
		.for_each_concurrent(
			num_cpus::get(), // number of logical CPUs available on machine
			// run those many concurrent syncing lightweight tasks, not threads
			|(block_num, url, store, ipfs, pp)| async move {
				// TODO: Should we handle unprocessed blocks differently?
				if let Err(error) = process_block(
					url,
					store,
					block_num,
					ipfs,
					max_parallel_fetch_tasks,
					pp.clone(),
					confidence,
				)
				.await
				{
					error!("Cannot process block {block_num}: {error}");
				}
			},
		)
		.await;
}
