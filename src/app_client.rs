use std::sync::{mpsc::Receiver, Arc};

use anyhow::{anyhow, Context, Result};
use ipfs_embed::{DefaultParams, Ipfs};
use kate_recovery::com::{app_specific_cells, decode_app_extrinsics, DataCell, Position};
use rocksdb::DB;

use crate::{
	consts::APP_DATA_CF,
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
	positions: &Vec<Position>,
) -> Result<()> {
	let positions_count = positions.len();
	let block_number = block.number;
	let rows = block.dimensions.rows as u16;
	let cols = block.dimensions.cols as u16;
	let commitments = block.commitment.clone();

	log::info!("Found {positions_count} cells for app {app_id} in block {block_number}");

	let (ipfs_cells, unfetched) = fetch_cells_from_ipfs(ipfs, block.number, positions).await?;

	let ipfs_cells_count = ipfs_cells.len();
	log::info!("Fetched {ipfs_cells_count} cells of block {block_number} from IPFS");

	let rpc_cells = get_kate_proof(rpc_url, block.number, unfetched).await?;

	let rpc_cells_count = rpc_cells.len();
	log::info!("Fetched {rpc_cells_count} cells of block {block_number} from RPC");

	let cells = [ipfs_cells.as_slice(), rpc_cells.as_slice()].concat();

	let missing_cells_count = positions.len() - cells.len();
	if missing_cells_count > 0 {
		return Err(anyhow!("Failed to fetch {missing_cells_count} cells"));
	}

	let count = verify_proof(block_number, rows, cols, cells.clone(), commitments);
	log::info!("Completed {count} verification rounds for block {block_number}");

	if count < cells.len() {
		return Err(anyhow!("{} cells are not verified", cells.len() - count));
	}

	insert_into_ipfs(ipfs, block.number, rpc_cells).await;
	log::info!("Cells inserted into IPFS for block {}", block.number);

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
