use std::sync::{mpsc::Receiver, Arc};

use anyhow::{anyhow, Context, Result};
use ipfs_embed::{DefaultParams, Ipfs};
use kate_recovery::com::{app_specific_cells, decode_app_extrinsics};
use rocksdb::DB;

use crate::{
	consts::APP_DATA_CF,
	data::fetch_cells_from_ipfs,
	proof::verify_proof,
	rpc::get_kate_proof,
	types::{Cell, ClientMsg},
	util::layout_from_index,
};

async fn process_block(
	ipfs: &Ipfs<DefaultParams>,
	db: Arc<DB>,
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

	fn from_cells(cells: Vec<Cell>) -> Vec<kate_recovery::com::Cell> {
		cells
			.iter()
			.map(|cell| kate_recovery::com::Cell {
				col: cell.col,
				row: cell.row,
				data: cell.data.clone(),
			})
			.collect()
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
			cells.extend(rpc_fetched.clone());

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

			// TODO: If there are some invalid cells should we fail?

			for cell in rpc_fetched {
				if let Err(error) = ipfs.insert(cell.clone().to_ipfs_block()) {
					log::info!(
						"Error pushing cell to IPFS: {}. Cell reference: {}",
						error,
						cell.reference()
					);
				}
				// Add generated CID to DHT
				if let Err(error) = ipfs
					.put_record(cell.ipfs_record(), ipfs_embed::Quorum::One)
					.await
				{
					log::info!(
						"Error inserting new record to DHT: {}. Cell reference: {}",
						error,
						cell.reference()
					);
				}
			}
			if let Err(error) = ipfs.flush().await {
				log::info!("Error flushin data do disk: {}", error,);
			};

			let decoded =
				decode_app_extrinsics(layout, &block.dimensions, from_cells(cells), app_id)?;

			let key = format!("{}:{}", app_id, &block.number);

			let cf_handle = db
				.cf_handle(APP_DATA_CF)
				.context("failed to get cf handle")?;

			db.put_cf(
				&cf_handle,
				key.as_bytes(),
				serde_json::to_string(&decoded)?.as_bytes(),
			)
			.context("failed to write application data")?;

			log::info!(
				"Stored {} data into database",
				decoded.iter().map(|(_, data)| data.len()).sum::<usize>()
			);
		},
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
		if let Err(error) = process_block(&ipfs, db.clone(), &rpc_url, app_id, &block).await {
			log::error!("Cannot process block {}: {}", block.number, error);
			continue;
		}
	}
}
