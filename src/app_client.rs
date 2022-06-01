use std::sync::mpsc::Receiver;

use anyhow::{anyhow, Result};
use ipfs_embed::{DefaultParams, Ipfs};
use kate_recovery::com::app_specific_cells;

use crate::{
	data::fetch_cells_from_ipfs,
	proof::verify_proof,
	rpc::get_kate_proof,
	types::{Cell, ClientMsg},
	util::layout_from_index,
};

async fn process_block(
	ipfs: &Ipfs<DefaultParams>,
	rpc_url: &str,
	app_id: u32,
	block: &ClientMsg,
) -> Result<()> {
	let layout = &layout_from_index(&block.lookup.index, block.lookup.size);

	fn to_cells(block_number: u64, positions: Vec<kate_recovery::com::Cell>) -> Vec<Cell> {
		positions
			.iter()
			.map(|cell| Cell::position(block_number, cell.row, cell.col))
			.collect::<Vec<_>>()
	}

	match app_specific_cells(layout, &block.dimensions, app_id)
		.map(move |positions| to_cells(block.number, positions))
	{
		None => log::info!("No cells for app {} in block {}", app_id, block.number),
		Some(positions) => {
			log::info!(
				"Found {} cells for app {} in block {}",
				positions.len(),
				app_id,
				block.number
			);

			let (ipfs_fetched, unfetched) =
				fetch_cells_from_ipfs(ipfs, block.number, &positions).await?;

			log::info!(
				"Number of cells fetched from IPFS for block {}: {}",
				block.number,
				ipfs_fetched.len()
			);

			let rpc_fetched = get_kate_proof(rpc_url, block.number, unfetched).await?;

			log::info!(
				"Number of cells fetched from RPC for block {}: {}",
				block.number,
				rpc_fetched.len()
			);

			let mut cells = vec![];
			cells.extend(ipfs_fetched);
			cells.extend(rpc_fetched);

			if positions.len() > cells.len() {
				return Err(anyhow!(
					"Failed to fetch {} cells",
					positions.len() - cells.len()
				));
			}

			let count = verify_proof(
				block.number,
				block.dimensions.rows as u16,
				block.dimensions.cols as u16,
				cells.clone(),
				block.commitment.clone(),
			);
			log::info!(
				"Completed {} verification rounds for block {}",
				count,
				block.number
			);
		},
	}
	Ok(())
}

pub async fn run(
	ipfs: Ipfs<DefaultParams>,
	rpc_url: String,
	app_id: u32,
	block_receive: Receiver<ClientMsg>,
) {
	log::info!("Starting for app {}...", app_id);

	for block in block_receive {
		log::info!("Block {} available", block.number);
		if let Err(error) = process_block(&ipfs, &rpc_url, app_id, &block).await {
			log::error!("Cannot process block {}: {}", block.number, error);
			continue;
		}
	}
}
