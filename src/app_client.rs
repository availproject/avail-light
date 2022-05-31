use std::sync::mpsc::Receiver;

use crate::types::ClientMsg;

pub async fn run(app_id: u16, block_receive: Receiver<ClientMsg>) {
	log::info!("Starting for app {}...", app_id);

	for block in block_receive {
		log::debug!("Block {} available", block.num);
	}
}
