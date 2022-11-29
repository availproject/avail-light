//! Application client for data fetching and reconstruction.
//!
//! App client is enabled when app_id is configured and greater than 0 in avail-light configuration. [`Light client`](super::light_client) triggers application client if block is verified with high enough confidence. Currently [`run`] function is separate task and doesn't block main thread.
//!
//! # Flow
//!
//! Get app data rows from node
//! Verify commitment equality for each row
//! Decode app data and store it into local database under the `app_id:block_number` key
//!
//! # Notes
//!
//! If application client fails to run or stops its execution, error is logged, and other tasks continue with execution.

use anyhow::{Context, Result};
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use kate_recovery::{
	com::decode_app_extrinsics,
	commitments,
	config::CHUNK_SIZE,
	data::{self, DataCell},
	matrix::Position,
};
use rocksdb::DB;
use std::sync::{mpsc::Receiver, Arc};
use tracing::{error, info, instrument, warn};

use crate::{
	data::{fetch_cells_from_dht, store_encoded_data_in_db},
	network::Client,
	proof,
	rpc::get_kate_app_data,
	types::{AppClientConfig, ClientMsg},
};

fn new_data_cell(row: usize, col: usize, data: &[u8]) -> Result<DataCell> {
	Ok(DataCell {
		position: Position {
			row: row.try_into()?,
			col: col.try_into()?,
		},
		data: data.try_into()?,
	})
}

fn data_cells_from_row(row: usize, row_data: &[u8]) -> Result<Vec<DataCell>> {
	row_data
		.chunks_exact(CHUNK_SIZE)
		.enumerate()
		.map(move |(col, data)| new_data_cell(row, col, data))
		.collect::<Result<Vec<DataCell>>>()
}

fn data_cells_from_rows(rows: Vec<Option<Vec<u8>>>) -> Result<Vec<DataCell>> {
	Ok(rows
		.into_iter()
		.enumerate() // Add row indexes
		.filter_map(|(row, row_data)| row_data.map(|data| (row, data))) // Remove None rows
		.map(|(row, data)| data_cells_from_row(row, &data))
		.collect::<Result<Vec<Vec<DataCell>>, _>>()?
		.into_iter()
		.flatten()
		.collect::<Vec<_>>())
}

#[instrument(skip_all, fields(block = block.block_num), level = "trace")]
async fn process_block(
	cfg: &AppClientConfig,
	db: Arc<DB>,
	network_client: Client,
	rpc_url: &str,
	app_id: u32,
	block: &ClientMsg,
	pp: PublicParameters,
	thresh: usize,
) -> Result<()> {
	let lookup = &block.lookup;
	let block_number = block.block_num;
	let dimensions = &block.dimensions;
	let commitments = &block.commitment;

	let mut rows = get_kate_app_data(rpc_url, block.header_hash, app_id).await?;

	let rows_count = rows.iter().filter(|row| row.is_some()).count();
	info!(block_number, "Found {rows_count} rows for app {app_id}");

	let (verified_rows, missing_rows) =
		commitments::verify_equality(&pp, &commitments, &rows, &lookup, &dimensions, app_id)?;

	info!(block_number, "Verified rows: {verified_rows:?}");

	if missing_rows.len() * dimensions.cols() as usize > thresh {
		return Err(anyhow::anyhow!("Too many cells are missing"));
	}

	let mut have_missing = !missing_rows.is_empty();

	if have_missing {
		let missing_positions = dimensions.extended_rows_positions(&missing_rows);
		let (fetched, unfetched) = fetch_cells_from_dht(
			&network_client,
			block_number,
			&missing_positions,
			cfg.dht_parallelization_limit,
		)
		.await
		.context("Failed to fetch cells from DHT")?;

		let unfetched_count = unfetched.len();
		if unfetched_count == 0 {
			let verified =
				proof::verify_proof(block_number, &dimensions, &fetched, commitments.clone(), pp);

			let unverified = fetched.len() - verified;
			if unverified == 0 {
				// Assumption is that correct cells are fetched from DHT
				for (row, data) in data::rows(&fetched) {
					rows[row as usize] = Some(data);
				}
				have_missing = false;
			} else {
				warn!(block_number, "Failed to verify {unverified} DHT cells",);
			}
		} else {
			warn!(block_number, "Failed to fetch {unfetched_count} DHT cells",);
		}
	};

	if !have_missing {
		let data_cells = data_cells_from_rows(rows)
			.context("Failed to create data cells from rows got from RPC")?;

		let data = decode_app_extrinsics(&lookup, &dimensions, data_cells, app_id)
			.context("Failed to decode app extrinsics")?;

		store_encoded_data_in_db(db, app_id, block_number, &data)
			.context("Failed to store data into database")?;

		let bytes_count = data.iter().fold(0usize, |acc, x| acc + x.len());
		info!(block_number, "Stored {bytes_count} bytes into database",);
	} else {
		warn!(block_number, "Failed to fetch and verify data");
	}
	Ok(())
}

/// Runs application client.
///
/// # Arguments
///
/// * `cfg` - Application client configuration
/// * `db` - Database to store data inot DB
/// * `network_client` - Reference to a libp2p custom network client
/// * `rpc_url` - Node's RPC URL for fetching data unavailable in DHT (if configured)
/// * `app_id` - Application ID
/// * `block_receive` - Channel used to receive header of verified block
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
pub async fn run(
	cfg: AppClientConfig,
	db: Arc<DB>,
	network_client: Client,
	rpc_url: String,
	app_id: u32,
	block_receive: Receiver<ClientMsg>,
	pp: PublicParameters,
	thresh: usize,
) {
	info!("Starting for app {app_id}...");

	for block in block_receive {
		let block_number = block.block_num;

		info!(block_number, "Block available");

		if block.dimensions.cols() == 0 {
			continue;
		}

		if block
			.lookup
			.index
			.iter()
			.filter(|&(id, _)| id == &app_id)
			.count() == 0
		{
			info!(block_number, "No cells for app {app_id}");
			continue;
		}

		if let Err(error) = process_block(
			&cfg,
			db.clone(),
			network_client.clone(),
			&rpc_url,
			app_id,
			&block,
			pp.clone(),
			thresh,
		)
		.await
		{
			error!(block_number, "Cannot process block: {error}");
		}
	}
}
