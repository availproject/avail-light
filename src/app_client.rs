use std::{
	collections::HashSet,
	sync::{mpsc::Receiver, Arc},
};

use anyhow::{anyhow, Result};
use ipfs_embed::{DefaultParams, Ipfs};
use kate_recovery::com::{
	app_specific_cells, app_specific_column_cells, decode_app_extrinsics,
	reconstruct_app_extrinsics, Cell, DataCell, ExtendedMatrixDimensions, Position,
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
	column_positions: &[Position],
) -> Result<()> {
	let block_number = block.number;

	log::info!(
		"Found {count} cells for app {app_id} in block {block_number}",
		count = data_positions.len()
	);

	let (mut ipfs_cells, unfetched) =
		fetch_cells_from_ipfs(ipfs, block.number, data_positions).await?;

	log::info!(
		"Fetched {count} cells of block {block_number} from IPFS",
		count = ipfs_cells.len()
	);

	let mut rpc_cells = get_kate_proof(rpc_url, block.number, unfetched).await?;

	log::info!(
		"Fetched {count} cells of block {block_number} from RPC",
		count = rpc_cells.len()
	);

	let reconstruct = data_positions.len() > (ipfs_cells.len() + rpc_cells.len());

	if reconstruct {
		let fetched = [ipfs_cells.as_slice(), rpc_cells.as_slice()].concat();
		let column_positions = diff_positions(column_positions, &fetched);
		let (mut column_ipfs_cells, unfetched) =
			fetch_cells_from_ipfs(ipfs, block_number, &column_positions).await?;

		log::info!(
			"Fetched {count} cells of block {block_number} from IPFS for reconstruction",
			count = column_ipfs_cells.len()
		);

		ipfs_cells.append(&mut column_ipfs_cells);
		let fetched = [ipfs_cells.as_slice(), rpc_cells.as_slice()].concat();
		if !can_reconstruct(&block.dimensions, &column_positions, &fetched) {
			let mut column_rpc_cells = get_kate_proof(rpc_url, block_number, unfetched).await?;

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

	insert_into_ipfs(ipfs, block.number, rpc_cells).await;

	log::info!("Cells inserted into IPFS for block {block_number}");

	let layout = &layout_from_index(&block.lookup.index, block.lookup.size);
	let data_cells = cells.into_iter().map(DataCell::from).collect::<Vec<_>>();
	let data = if reconstruct {
		reconstruct_app_extrinsics(layout, &block.dimensions, data_cells, app_id)?
	} else {
		decode_app_extrinsics(layout, &block.dimensions, data_cells, app_id)?
	};

	store_data_in_db(db, app_id, block_number, &data)?;
	log::info!("Stored {count} data into database", count = data.len());
	Ok(())
}

fn can_reconstruct(
	dimensions: &ExtendedMatrixDimensions,
	column_positions: &[Position],
	cells: &[Cell],
) -> bool {
	let columns = column_positions.iter().map(|position| position.col);
	HashSet::<u16>::from_iter(columns).iter().all(|&col| {
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
