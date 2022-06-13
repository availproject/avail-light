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
	consts,
	data::{fetch_cells_from_ipfs, insert_into_ipfs},
	http::calculate_confidence,
	proof, rpc,
	types::{self, ClientMsg},
};

pub async fn run(
	full_node_ws: Vec<String>,
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
			.context("ws-message(chain_subscribeFinalizedHeads) send failed")?;

		log::info!("Connected to Substrate Node");

		let db_3 = db.clone();
		let cf_handle_0 = db_3
			.cf_handle(consts::CONFIDENCE_FACTOR_CF)
			.context("failed to get cf handle")?;
		let cf_handle_1 = db_3
			.cf_handle(consts::BLOCK_HEADER_CF)
			.context("failed to get cf handle")?;

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

					let positions = rpc::generate_random_cells(max_rows, max_cols);
					log::info!(
						"Random cells generated: {} from block: {}",
						positions.len(),
						num
					);

					let (ipfs_fetched, unfetched) =
						fetch_cells_from_ipfs(&ipfs, num, &positions, max_parallel_fetch_tasks)
							.await?;

					log::info!(
						"Number of cells fetched from IPFS for block {}: {}",
						num,
						ipfs_fetched.len()
					);

					let rpc_fetched = rpc::get_kate_proof(&rpc_url, num, unfetched).await?;

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
						"Completed {} verification rounds for block {}\t{:?}",
						count,
						num,
						begin
							.elapsed()
							.context("failed to get complete verification")?
					);

					// write confidence factor into on-disk database
					db_3.put_cf(&cf_handle_0, num.to_be_bytes(), count.to_be_bytes())
						.context("failed to write confidence factor")?;

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
					db_3.put_cf(
						&cf_handle_1,
						num.to_be_bytes(),
						serde_json::to_string(&header)?.as_bytes(),
					)
					.context("failed to write block header")?;

					insert_into_ipfs(&ipfs, num, rpc_fetched).await;
					log::info!("Cells inserted into IPFS for block {num}");

					// notify ipfs-based application client
					// that newly mined block has been received
					block_tx
						.send(types::ClientMsg::from(params.header))
						.context("failed to send block message")?;
				},
				Err(error) => log::info!("Misconstructed Header: {:?}", error),
			}
		}
	}
	Ok(())
}
