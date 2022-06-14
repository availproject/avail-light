use std::sync::{mpsc::Receiver, Arc};

use anyhow::{anyhow, Result};
use ipfs_embed::{DefaultParams, Ipfs};
use kate_recovery::com::{
	app_specific_cells, app_specific_column_cells, decode_app_extrinsics,
	reconstruct_app_extrinsics, DataCell, Position,
};
use rocksdb::DB;

use crate::{
	data::{fetch_cells_from_ipfs, insert_into_ipfs, store_data_in_db},
	proof::verify_proof,
	rpc::get_kate_proof,
	types::ClientMsg,
	util::layout_from_index,
};

async fn process_block(
	ipfs: &Ipfs<DefaultParams>,
	db: Arc<DB>,
	rpc_url: &str,
	app_id: u32,
	block: &ClientMsg,
	data_positions: &Vec<Position>,
	column_positions: &Vec<Position>,
) -> Result<()> {
	let data_positions_count = data_positions.len();
	let block_number = block.number;
	let dimensions = &block.dimensions;
	let rows = block.dimensions.rows as u16;
	let cols = block.dimensions.cols as u16;
	let commitments = block.commitment.clone();

	log::info!("Found {data_positions_count} cells for app {app_id} in block {block_number}");

	let (ipfs_cells, unfetched) = fetch_cells_from_ipfs(ipfs, block.number, data_positions).await?;

	let ipfs_cells_count = ipfs_cells.len();
	log::info!("Fetched {ipfs_cells_count} cells of block {block_number} from IPFS");

	let mut rpc_cells = get_kate_proof(rpc_url, block.number, unfetched).await?;

	let rpc_cells_count = rpc_cells.len();
	log::info!("Fetched {rpc_cells_count} cells of block {block_number} from RPC");

	let cells = [ipfs_cells.as_slice(), rpc_cells.as_slice()].concat();

	let missing_cells_count = data_positions.len() - cells.len();
	if missing_cells_count == 0 {
		let count = verify_proof(block_number, rows, cols, cells.clone(), commitments);
		log::info!("Completed {count} verification rounds for block {block_number}");

		if count < cells.len() {
			return Err(anyhow!("{} cells are not verified", cells.len() - count));
		}

		insert_into_ipfs(ipfs, block.number, rpc_cells).await;
		log::info!("Cells inserted into IPFS for block {}", block.number);

		let layout = &layout_from_index(&block.lookup.index, block.lookup.size);
		let data_cells = cells.into_iter().map(DataCell::from).collect::<Vec<_>>();
		let data = decode_app_extrinsics(layout, dimensions, data_cells, app_id)?;

		let data_count = data.len();
		store_data_in_db(db, app_id, block_number, data)?;
		log::info!("Stored {data_count} data into database");
	} else {
		let diff_positions = column_positions
			.iter()
			.cloned()
			.filter(|position| !cells.iter().any(|cell| cell.position.eq(position)))
			.collect::<Vec<_>>();

		let (ipfs_cells, unfetched) =
			fetch_cells_from_ipfs(ipfs, block.number, &diff_positions).await?;

		let ipfs_cells_count = ipfs_cells.len();
		log::info!("Fetched {ipfs_cells_count} cells of block {block_number} from IPFS");

		let mut column_rpc_cells = get_kate_proof(rpc_url, block.number, unfetched).await?;

		let column_rpc_cells_count = column_rpc_cells.len();
		log::info!("Fetched {column_rpc_cells_count} cells of block {block_number} from RPC");

		let cells = [
			cells.as_slice(),
			ipfs_cells.as_slice(),
			column_rpc_cells.as_slice(),
		]
		.concat();

		let count = verify_proof(block_number, rows, cols, cells.clone(), commitments);
		log::info!("Completed {count} verification rounds for block {block_number}");

		if count < cells.len() {
			return Err(anyhow!("{} cells are not verified", cells.len() - count));
		}

		rpc_cells.append(&mut column_rpc_cells);
		insert_into_ipfs(ipfs, block.number, rpc_cells).await;
		log::info!("Cells inserted into IPFS for block {}", block.number);

		let layout = &layout_from_index(&block.lookup.index, block.lookup.size);
		let data_cells = cells.into_iter().map(DataCell::from).collect::<Vec<_>>();
		let data = reconstruct_app_extrinsics(layout, dimensions, data_cells, app_id)?;

		let data_count = data.len();
		store_data_in_db(db, app_id, block_number, data)?;
		log::info!("Stored {data_count} data into database");
	}

	Ok(())
}

pub async fn run(
	ipfs: Ipfs<DefaultParams>,
	db: Arc<DB>,
	rpc_url: String,
	app_id: u32,
	block_receive: Receiver<ClientMsg>,
) {
	log::info!("Starting for app {}...", app_id);

	for block in block_receive {
		log::info!("Block {} available", block.number);
		let layout = &layout_from_index(&block.lookup.index, block.lookup.size);

		match (
			app_specific_cells(layout, &block.dimensions, app_id),
			app_specific_column_cells(layout, &block.dimensions, app_id),
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
				)
				.await
				{
					log::error!("Cannot process block {}: {}", block.number, error);
					continue;
				}
			},
			(_, _) => log::info!("No cells for app {} in block {}", app_id, block.number),
		}
	}
}
