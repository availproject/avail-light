use std::sync::mpsc::Receiver;

use kate_recovery::com::app_specific_cells;

use crate::{types::ClientMsg, util::layout_from_index};

pub async fn run(app_id: u32, block_receive: Receiver<ClientMsg>) {
	log::info!("Starting for app {}...", app_id);

	for block in block_receive {
		log::info!("Block {} available", block.number);
		let layout = &layout_from_index(&block.lookup.index, block.lookup.size);
		match app_specific_cells(layout, &block.dimensions, app_id) {
			None => log::info!("No cells for app {} in block {}", app_id, block.number),
			Some(cells) => log::info!(
				"Found {} cells for app {} in block {}",
				cells.len(),
				app_id,
				block.number
			),
		}
	}
}
