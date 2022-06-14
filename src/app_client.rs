use std::sync::{mpsc::Receiver, Arc};

use anyhow::{anyhow, Context, Result};
use ipfs_embed::{DefaultParams, Ipfs};
use kate_recovery::com::{app_specific_cells, decode_app_extrinsics, DataCell, Position};
use rocksdb::DB;

use crate::{
	consts::APP_DATA_CF,
	data::fetch_cells_from_ipfs,
	proof::verify_proof,
	rpc::get_kate_proof,
	types::{cell_ipfs_record, cell_to_ipfs_block, ClientMsg},
	util::layout_from_index,
};

async fn process_block(
	ipfs: &Ipfs<DefaultParams>,
	db: Arc<DB>,
	rpc_url: &str,
	app_id: u32,
	block: &ClientMsg,
	positions: &Vec<Position>,
) -> Result<()> {
	log::info!(
		"Found {} cells for app {} in block {}",
		positions.len(),
		app_id,
		block.number
	);

	let (ipfs_fetched, unfetched) = fetch_cells_from_ipfs(ipfs, block.number, positions).await?;

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

	if count < cells.len() {
		return Err(anyhow!("{} cells are not verified", cells.len() - count));
	}

	// TODO: If there are some invalid cells should we fail?

	for cell in rpc_fetched {
		if let Err(error) = ipfs.insert(cell_to_ipfs_block(cell.clone())) {
			log::info!(
				"Error pushing cell to IPFS: {}. Cell reference: {}",
				error,
				cell.reference(block.number)
			);
		}
		// Add generated CID to DHT
		if let Err(error) = ipfs
			.put_record(
				cell_ipfs_record(&cell, block.number),
				ipfs_embed::Quorum::One,
			)
			.await
		{
			log::info!(
				"Error inserting new record to DHT: {}. Cell reference: {}",
				error,
				cell.reference(block.number)
			);
		}
	}
	if let Err(error) = ipfs.flush().await {
		log::info!("Error flushin data do disk: {}", error,);
	};

	let layout = &layout_from_index(&block.lookup.index, block.lookup.size);
	let data_cells = cells.into_iter().map(DataCell::from).collect::<Vec<_>>();
	// TODO: Return Result<Vec<Vec<u8>> instead
	let decoded = decode_app_extrinsics(layout, &block.dimensions, data_cells, app_id)?;
	if decoded.len() != 1 {
		return Ok(());
	}
	let data = &decoded[0].1;

	let key = format!("{}:{}", app_id, &block.number);

	let cf_handle = db
		.cf_handle(APP_DATA_CF)
		.context("failed to get cf handle")?;

	db.put_cf(
		&cf_handle,
		key.as_bytes(),
		serde_json::to_string(&data)?.as_bytes(),
	)
	.context("failed to write application data")?;

	log::info!("Stored {} data into database", data.len());

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

		match app_specific_cells(layout, &block.dimensions, app_id) {
			None => log::info!("No cells for app {} in block {}", app_id, block.number),
			Some(positions) => {
				if let Err(error) =
					process_block(&ipfs, db.clone(), &rpc_url, app_id, &block, &positions).await
				{
					log::error!("Cannot process block {}: {}", block.number, error);
					continue;
				}
			},
		}
	}
}
