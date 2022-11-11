//! Application client for data fetching and reconstruction.
//!
//! App client is enabled when app_id is configured and greater than 0 in avail-light configuration. [`Light client`](super::light_client) triggers application client if block is verified with high enough confidence. Currently [`run`] function is separate task and doesn't block main thread.
//!
//! # Flow
//!
//! * Download app specific data cells from DHT
//! * Download missing app specific data cells from the full node
//! * If some cells are still missing
//!     * Download related columns from IPFS (excluding downloaded cells)
//!     * If reconstruction with downloaded cells is not possible, download related columns from full node (excluding downloaded cells)
//! * Verify downloaded data cells
//! * Insert cells downloaded from full node into DHT
//! * Decode (or reconstruct if app specific data cells are missing), and store it into local database under the `app_id:block_number` key
//!
//! # Notes
//!
//! If application client fails to run or stops its execution, error is logged, and other tasks continue with execution.

use anyhow::{Context, Result};
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;

use kate_recovery::{
	com::decode_app_extrinsics, config::CHUNK_SIZE, data::DataCell, matrix::Position,
};
use rocksdb::DB;
use std::sync::{mpsc::Receiver, Arc};
use tracing::{error, info, instrument};

use crate::{
	data::store_encoded_data_in_db,
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
	db: Arc<DB>,
	rpc_url: &str,
	app_id: u32,
	block: &ClientMsg,
	pp: PublicParameters,
) -> Result<()> {
	let block_number = block.block_num;
	let commitments = &block.commitment;

	let rows = get_kate_app_data(rpc_url, block.header_hash, app_id).await?;
	let rows_count = rows.iter().filter(|&o| Option::is_some(o)).count();
	info!(block_number, "Found {rows_count} rows for app {app_id}");

	let is_verified =
		kate_recovery::commitments::verify_equality(&pp, commitments, &rows, &block.lookup, &block.dimensions, app_id)?;

	info!(block_number, "Block verified: {is_verified}");

	if is_verified {
		let data_cells = data_cells_from_rows(rows)
			.context("Failed to create data cells from rows got from RPC")?;

		let data = decode_app_extrinsics(&block.lookup, &block.dimensions, data_cells, app_id)
			.context("Failed to decode app extrinsics")?;

		store_encoded_data_in_db(db, app_id, block_number, &data)
			.context("Failed to store data into database")?;

		info!(
			block_number,
			"Stored {count} bytes into database",
			count = data.iter().fold(0usize, |acc, x| acc + x.len())
		);
	}

	Ok(())
}

/// Runs application client.
///
/// # Arguments
///
/// * `cfg` - Application client configuration
/// * `db` - Database to store data inot DB
/// * `rpc_url` - Node's RPC URL for fetching data unavailable in DHT (if configured)
/// * `app_id` - Application ID
/// * `block_receive` - Channel used to receive header of verified block
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
pub async fn run(
	_cfg: AppClientConfig,
	db: Arc<DB>,
	rpc_url: String,
	app_id: u32,
	block_receive: Receiver<ClientMsg>,
	pp: PublicParameters,
) {
	info!("Starting for app {app_id}...");

	for block in block_receive {
		let block_number = block.block_num;

		info!(block_number, "Block available");

		if block.dimensions.cols == 0 {
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

		if let Err(error) = process_block(db.clone(), &rpc_url, app_id, &block, pp.clone()).await {
			error!(block_number, "Cannot process block: {error}");
		}
	}
}
#[cfg(test)]
mod tests {
	use super::AvailExtrinsic;

	#[test]
	fn test_decode_xt() {
		let xt= serde_json::to_string("0xe9018400de1113c5912fda9c77305cddd98e2b5ca156f260ff2ac329dde67110854f8f3901007a35bdd5ec15a69bcd37d648dafcf18693f158baca512be44f1dfc218048581ba527938763b9b5a16f915e29c101c8450a2dd04d795de704f496ce9c81038d00dd1900030000001d01306578616d706c652064617461").unwrap();
		let x: AvailExtrinsic = serde_json::from_str(&xt).unwrap();
		let id = x.app_id;
		let data = String::from_utf8_lossy(x.data.as_slice());
		assert_eq!(id, 3);
		assert_eq!(data, "example data");
		println!("id: {id}, data: {data}.");
	}
}
