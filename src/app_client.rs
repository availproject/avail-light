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

use anyhow::{anyhow, Context, Result};
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use ipfs_embed::{DefaultParams, Ipfs};
use kate_recovery::com::{
	app_specific_cells, app_specific_column_cells, decode_app_extrinsics,
	reconstruct_app_extrinsics, Cell, DataCell, ExtendedMatrixDimensions, Position,
};
use rocksdb::DB;
use std::{
	collections::HashSet,
	sync::{mpsc::Receiver, Arc},
};
use tracing::{error, info};

use crate::{
	data::{fetch_cells_from_dht, insert_into_dht, store_encoded_data_in_db},
	proof::verify_proof,
	rpc::get_kate_proof,
	types::{AppClientConfig, ClientMsg},
};

async fn process_block(
	cfg: &AppClientConfig,
	ipfs: &Ipfs<DefaultParams>,
	db: Arc<DB>,
	rpc_url: &str,
	app_id: u32,
	block: &ClientMsg,
	data_positions: &Vec<Position>,
	column_positions: &[Position],
	pp: PublicParameters,
) -> Result<()> {
	let block_hash = block.header_hash;
	let block_number = block.block_num;
	info!(
		block.block_num,
		"Found {count} cells for app {app_id}",
		count = data_positions.len()
	);

	let (mut ipfs_cells, unfetched) = fetch_cells_from_dht(
		ipfs,
		block.block_num,
		data_positions,
		cfg.dht_parallelization_limit,
	)
	.await
	.context("Failed to fetch data cells from DHT")?;

	info!(
		block_number,
		"Fetched {count} cells from DHT",
		count = ipfs_cells.len()
	);

	let mut rpc_cells: Vec<Cell> = Vec::new();
	if !cfg.disable_rpc {
		for cell in unfetched.chunks(30) {
			let mut query_cells = get_kate_proof(rpc_url, block_hash, (*cell).to_vec())
				.await
				.context("Failed to fetch data cells from node RPC")?;

			rpc_cells.append(&mut query_cells);
		}
	}
	info!(
		block_number,
		"Fetched {count} cells from RPC",
		count = rpc_cells.len()
	);
	let reconstruct = data_positions.len() > (ipfs_cells.len() + rpc_cells.len());

	if reconstruct {
		let fetched = [ipfs_cells.as_slice(), rpc_cells.as_slice()].concat();
		let column_positions = diff_positions(column_positions, &fetched);
		let (mut column_ipfs_cells, unfetched) = fetch_cells_from_dht(
			ipfs,
			block_number,
			&column_positions,
			cfg.dht_parallelization_limit,
		)
		.await
		.context("Failed to fetch column cells from IPFS")?;

		info!(
			block_number,
			"Fetched {count} cells from IPFS for reconstruction",
			count = column_ipfs_cells.len()
		);

		ipfs_cells.append(&mut column_ipfs_cells);
		let columns = columns(&column_positions);
		let fetched = [ipfs_cells.as_slice(), rpc_cells.as_slice()].concat();
		if !can_reconstruct(&block.dimensions, &columns, &fetched) && !cfg.disable_rpc {
			let mut column_rpc_cells = get_kate_proof(rpc_url, block_hash, unfetched)
				.await
				.context("Failed to get column cells from node RPC")?;

			info!(
				block_number,
				"Fetched {count} cells from RPC for reconstruction",
				count = column_rpc_cells.len()
			);

			rpc_cells.append(&mut column_rpc_cells);
		}
	};

	let cells = [ipfs_cells.as_slice(), rpc_cells.as_slice()].concat();

	let verified = verify_proof(
		block_number,
		block.dimensions.rows as u16,
		block.dimensions.cols as u16,
		&cells,
		block.commitment.clone(),
		pp,
	);

	info!(block_number, "Completed {verified} verification rounds");

	if cells.len() > verified {
		return Err(anyhow!("{} cells are not verified", cells.len() - verified));
	}

	insert_into_dht(
		ipfs,
		block_number,
		rpc_cells,
		cfg.dht_parallelization_limit,
		cfg.ttl,
	)
	.await;

	info!(block_number, "Cells inserted into IPFS");

	let data_cells = cells.into_iter().map(DataCell::from).collect::<Vec<_>>();
	let data = if reconstruct {
		reconstruct_app_extrinsics(&block.lookup, &block.dimensions, data_cells, app_id)
			.map_err(|e| anyhow!("{}", e.to_string()))
			.context("Failed to reconstruct app extrinsics")?
	} else {
		decode_app_extrinsics(&block.lookup, &block.dimensions, data_cells, app_id)
			.map_err(|e| anyhow!("{}", e.to_string()))
			.context("Failed to decode app extrinsics")?
	};

	store_encoded_data_in_db(db, app_id, block_number, &data)
		.context("Failed to store data into database")?;
	info!(
		block_number,
		"Stored {count} bytes into database",
		count = data.iter().fold(0usize, |acc, x| acc + x.len())
	);
	Ok(())
}

fn columns(positions: &[Position]) -> Vec<u16> {
	let columns = positions.iter().map(|position| position.col);
	HashSet::<u16>::from_iter(columns)
		.into_iter()
		.collect::<Vec<u16>>()
}

fn can_reconstruct(dimensions: &ExtendedMatrixDimensions, columns: &[u16], cells: &[Cell]) -> bool {
	columns.iter().all(|&col| {
		cells
			.iter()
			.filter(move |cell| cell.position.col == col)
			.count() >= dimensions.rows / 2
	})
}

fn diff_positions(positions: &[Position], cells: &[Cell]) -> Vec<Position> {
	positions
		.iter()
		.cloned()
		.filter(|position| !cells.iter().any(|cell| cell.position.eq(position)))
		.collect::<Vec<_>>()
}

/// Runs application client.
///
/// # Arguments
///
/// * `cfg` - Application client configuration
/// * `ipfs` - IPFS instance to fetch and insert cells into DHT
/// * `db` - Database to store data inot DB
/// * `rpc_url` - Node's RPC URL for fetching data unavailable in DHT (if configured)
/// * `app_id` - Application ID
/// * `block_receive` - Channel used to receive header of verified block
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
pub async fn run(
	cfg: AppClientConfig,
	ipfs: Ipfs<DefaultParams>,
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

		match (
			app_specific_cells(&block.lookup, &block.dimensions, app_id),
			app_specific_column_cells(&block.lookup, &block.dimensions, app_id),
		) {
			(Some(data_positions), Some(column_positions)) => {
				if let Err(error) = process_block(
					&cfg,
					&ipfs,
					db.clone(),
					&rpc_url,
					app_id,
					&block,
					&data_positions,
					&column_positions,
					pp.clone(),
				)
				.await
				{
					error!(block_number, "Cannot process block: {error}");
					continue;
				}
			},
			(_, _) => info!(block_number, "No cells for app {app_id}"),
		}
	}
}
#[cfg(test)]
mod tests {
	use kate_recovery::com::{Cell, ExtendedMatrixDimensions, Position};

	use super::{can_reconstruct, diff_positions};

	fn position(row: u16, col: u16) -> Position {
		Position { row, col }
	}

	fn empty_cell(row: u16, col: u16) -> Cell {
		Cell {
			position: Position { row, col },
			content: [0u8; 80],
		}
	}

	#[test]
	fn test_can_reconstruct() {
		let dimensions = ExtendedMatrixDimensions { rows: 2, cols: 4 };
		let columns = vec![0, 1];
		let cells = vec![empty_cell(0, 0), empty_cell(0, 1)];
		assert!(can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 0), empty_cell(0, 1)];
		assert!(can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 0), empty_cell(1, 1)];
		assert!(can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(0, 0), empty_cell(1, 1)];
		assert!(can_reconstruct(&dimensions, &columns, &cells));
	}

	#[test]
	fn test_cannot_reconstruct() {
		let dimensions = ExtendedMatrixDimensions { rows: 2, cols: 4 };
		let columns = vec![0, 1];
		let cells = vec![empty_cell(0, 0)];
		assert!(!can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(0, 1)];
		assert!(!can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 0)];
		assert!(!can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 1)];
		assert!(!can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(0, 2), empty_cell(0, 3)];
		assert!(!can_reconstruct(&dimensions, &columns, &cells));
	}

	#[test]
	fn test_diff_positions() {
		let positions = vec![position(0, 0), position(1, 1)];
		let cells = vec![empty_cell(0, 0), empty_cell(1, 1)];
		assert_eq!(diff_positions(&positions, &cells).len(), 0);

		let positions = vec![position(0, 0), position(1, 1)];
		let cells = vec![empty_cell(0, 0), empty_cell(0, 1)];
		assert_eq!(diff_positions(&positions, &cells).len(), 1);
		assert_eq!(diff_positions(&positions, &cells)[0], position(1, 1));

		let positions = vec![position(0, 0), position(1, 1)];
		let cells = vec![empty_cell(1, 0), empty_cell(0, 1)];
		assert_eq!(diff_positions(&positions, &cells).len(), 2);
		assert_eq!(diff_positions(&positions, &cells)[0], position(0, 0));
		assert_eq!(diff_positions(&positions, &cells)[1], position(1, 1));
	}
}
