use std::{
	collections::HashSet,
	sync::{mpsc::Receiver, Arc},
};

use anyhow::{anyhow, Context, Result};
use ipfs_embed::{DefaultParams, Ipfs};
use kate_recovery::com::{
	app_specific_cells, app_specific_column_cells, decode_app_extrinsics,
	reconstruct_app_extrinsics, Cell, DataCell, ExtendedMatrixDimensions, Position,
};
use rocksdb::DB;

use crate::{
	data::{fetch_cells_from_dht, insert_into_dht, store_data_in_db},
	proof::verify_proof,
	rpc::get_kate_proof,
	types::ClientMsg,
};

async fn process_block(
	ipfs: &Ipfs<DefaultParams>,
	db: Arc<DB>,
	rpc_url: &str,
	app_id: u32,
	block: &ClientMsg,
	data_positions: &Vec<Position>,
	column_positions: &[Position],
	max_parallel_fetch_tasks: usize,
) -> Result<()> {
	let block_number = block.number;

	log::info!(
		"Found {count} cells for app {app_id} in block {block_number}",
		count = data_positions.len()
	);

	let (mut ipfs_cells, unfetched) =
		fetch_cells_from_dht(ipfs, block.number, data_positions, max_parallel_fetch_tasks)
			.await
			.context("Failed to fetch data cells from IPFS")?;

	log::info!(
		"Fetched {count} cells of block {block_number} from IPFS",
		count = ipfs_cells.len()
	);

	let mut rpc_cells = get_kate_proof(rpc_url, block.number, unfetched)
		.await
		.context("Failed to fetch data cells from node RPC")?;

	log::info!(
		"Fetched {count} cells of block {block_number} from RPC",
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
			max_parallel_fetch_tasks,
		)
		.await
		.context("Failed to fetch column cells from IPFS")?;

		log::info!(
			"Fetched {count} cells of block {block_number} from IPFS for reconstruction",
			count = column_ipfs_cells.len()
		);

		ipfs_cells.append(&mut column_ipfs_cells);
		let columns = columns(&column_positions);
		let fetched = [ipfs_cells.as_slice(), rpc_cells.as_slice()].concat();
		if !can_reconstruct(&block.dimensions, &columns, &fetched) {
			let mut column_rpc_cells = get_kate_proof(rpc_url, block_number, unfetched)
				.await
				.context("Failed to get column cells from node RPC")?;

			log::info!(
				"Fetched {count} cells of block {block_number} from RPC for reconstruction",
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
	);

	log::info!("Completed {verified} verification rounds for block {block_number}");

	if cells.len() > verified {
		return Err(anyhow!("{} cells are not verified", cells.len() - verified));
	}

	insert_into_dht(ipfs, block.number, rpc_cells).await;

	log::info!("Cells inserted into IPFS for block {block_number}");

	let data_cells = cells.into_iter().map(DataCell::from).collect::<Vec<_>>();
	let data = if reconstruct {
		reconstruct_app_extrinsics(&block.lookup, &block.dimensions, data_cells, app_id)
			.context("Failed to reconstruct app extrinsics")?
	} else {
		decode_app_extrinsics(&block.lookup, &block.dimensions, data_cells, app_id)
			.context("Failed to decode app extrinsics")?
	};

	store_data_in_db(db, app_id, block_number, &data)
		.context("Failed to store data into database")?;
	log::info!("Stored {count} data into database", count = data.len());
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

pub async fn run(
	ipfs: Ipfs<DefaultParams>,
	db: Arc<DB>,
	rpc_url: String,
	app_id: u32,
	block_receive: Receiver<ClientMsg>,
	max_parallel_fetch_tasks: usize,
) {
	log::info!("Starting for app {app_id}...");

	for block in block_receive {
		let block_number = block.number;
		log::info!("Block {block_number} available");

		match (
			app_specific_cells(&block.lookup, &block.dimensions, app_id),
			app_specific_column_cells(&block.lookup, &block.dimensions, app_id),
		) {
			(Some(data_positions), Some(column_positions)) => {
				if let Err(error) = process_block(
					&ipfs,
					db.clone(),
					&rpc_url,
					app_id,
					&block,
					&data_positions,
					&column_positions,
					max_parallel_fetch_tasks,
				)
				.await
				{
					log::error!("Cannot process block {block_number}: {error}");
					continue;
				}
			},
			(_, _) => log::info!("No cells for app {app_id} in block {block_number}"),
		}
	}
}

#[cfg(test)]
mod tests {
	use kate_recovery::com::{Cell, ExtendedMatrixDimensions, Position};

	use super::{can_reconstruct, diff_positions};

	fn position(row: u16, col: u16) -> Position { Position { row, col } }

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
