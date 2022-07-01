use std::{
	sync::{mpsc::SyncSender, Arc},
	time::SystemTime,
};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use ipfs_embed::{DefaultParams, Ipfs};
use rocksdb::DB;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::{
	data::{
		fetch_cells_from_dht, insert_into_dht, store_block_header_in_db, store_confidence_in_db,
	},
	http::calculate_confidence,
	proof, rpc,
	types::{self, ClientMsg},
};

pub async fn run(
	full_node_ws: Vec<String>,
	confidence: f64,
	db: Arc<DB>,
	ipfs: Ipfs<DefaultParams>,
	rpc_url: String,
	block_tx: SyncSender<ClientMsg>,
	max_parallel_fetch_tasks: usize,
) -> Result<()> {
	log::info!("Starting light client...");
	const BODY: &str = r#"{"id":1, "jsonrpc":"2.0", "method": "chain_subscribeFinalizedHeads"}"#;
	let urls = rpc::parse_urls(full_node_ws)?;
	while let Some(z) = rpc::check_connection(&urls).await {
		let (mut write, mut read) = z.split();
		write
			.send(Message::Text(BODY.to_string()))
			.await
			.context("Failed to send ws-message (chain_subscribeFinalizedHeads)")?;

		log::info!("Connected to Substrate Node");

		while let Some(message) = read.next().await {
			let data = message?.into_data();
			match serde_json::from_slice(&data) {
				Ok(types::Response { params, .. }) => {
					let header = &params.header;

					// now this is in `u64`
					let num = header.number;

					let begin = SystemTime::now();

					// TODO: Setting max rows * 2 to match extended matrix dimensions
					let max_rows = header.extrinsics_root.rows * 2;
					let max_cols = header.extrinsics_root.cols;
					if max_cols < 3 {
						log::error!("chunk size less than 3");
					}
					let commitment = header.extrinsics_root.commitment.clone();

					// let cell_count: u32;
					// if confidence == 100f64 {
					// 	cell_count = -((1f64 - 99.9f64 / 100f64).log2()).ceil() as u32;
					// } else {
					// 	cell_count = (-((1f64 - confidence / 100f64).log2())).ceil() as u32;
					// }
					let cell_count = rpc::cell_count_for_confidence(confidence);
					let positions = rpc::generate_random_cells(max_rows, max_cols, cell_count);
					log::info!(
						"Random cells generated: {} from block: {}",
						positions.len(),
						num
					);

					let (ipfs_fetched, unfetched) =
						fetch_cells_from_dht(&ipfs, num, &positions, max_parallel_fetch_tasks)
							.await
							.context("Failed to fetch cells from DHT")?;

					log::info!(
						"Number of cells fetched from DHT for block {}: {}",
						num,
						ipfs_fetched.len()
					);

					let rpc_fetched = rpc::get_kate_proof(&rpc_url, num, unfetched)
						.await
						.context("Failed to fetch cells from node RPC")?;

					log::info!(
						"Number of cells fetched from RPC for block {}: {}",
						num,
						rpc_fetched.len()
					);

					let mut cells = vec![];
					cells.extend(ipfs_fetched);
					cells.extend(rpc_fetched.clone());

					if positions.len() > cells.len() {
						log::error!("Failed to fetch {} cells", positions.len() - cells.len());
						continue;
					}

					let count =
						proof::verify_proof(num, max_rows, max_cols, &cells, commitment.clone());
					log::info!(
						"Completed {count} verification rounds for block {num}\t{:?}",
						begin.elapsed()?
					);

					// write confidence factor into on-disk database
					store_confidence_in_db(db.clone(), num, count as u32)
						.context("Failed to store confidence in DB")?;

					let conf = calculate_confidence(count as u32);
					log::info!("Confidence factor for block {}: {}", num, conf);

					// push latest mined block's header into column family specified
					// for keeping block headers, to be used
					// later for verifying IPFS stored data
					//
					// @note this same data store is also written to in
					// another competing thread, which syncs all block headers
					// in range [0, LATEST], where LATEST = latest block number
					// when this process started
					store_block_header_in_db(db.clone(), num, header)
						.context("Failed to store block header in DB")?;

					insert_into_dht(&ipfs, num, rpc_fetched).await;
					log::info!("Cells inserted into IPFS for block {num}");

					// notify ipfs-based application client
					// that newly mined block has been received
					block_tx
						.send(types::ClientMsg::from(params.header))
						.context("Failed to send block message")?;
				},
				Err(error) => log::info!("Misconstructed Header: {:?}", error),
			}
		}
	}
	Ok(())
}


#[cfg(test)]

mod tests{
	
	use super::rpc::cell_count_for_confidence;

	#[test]

	fn test_cell_count_for_confidence(){
		let count = 1;
		assert_eq!(cell_count_for_confidence(60f64)>count, true);
		assert_eq!(cell_count_for_confidence(100f64), (-((1f64 - (99f64/ 100f64)).log2())).ceil() as u32 );
		assert_eq!(cell_count_for_confidence(49f64), (-((1f64 - (99f64/ 100f64)).log2())).ceil() as u32 );
	}
}