use std::{
	sync::{
		mpsc::{sync_channel, SyncSender},
		Arc,
	},
	time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result};
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use futures_util::{SinkExt, StreamExt};
use ipfs_embed::{DefaultParams, Ipfs};
use prometheus::Registry;
use rocksdb::DB;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{error, info};

use crate::{
	data::{
		fetch_cells_from_dht, insert_into_dht, store_block_header_in_db, store_confidence_in_db,
	},
	http::calculate_confidence,
	proof, rpc,
	types::{self, ClientMsg, LightClientConfig, QueryResult},
};

pub async fn run(
	cfg: LightClientConfig,
	db: Arc<DB>,
	ipfs: Ipfs<DefaultParams>,
	rpc_url: String,
	block_tx: SyncSender<ClientMsg>,
	pp: PublicParameters,
	registry: Registry,
) -> Result<()> {
	info!("Starting light client...");
	const BODY: &str = r#"{"id":1, "jsonrpc":"2.0", "method": "chain_subscribeFinalizedHeads"}"#;
	let urls = rpc::parse_urls(&cfg.full_node_ws)?;
	// Register metrics
	let block_counter = Box::new(prometheus::Counter::new(
		"block_number",
		"Current block number",
	)?);
	registry
		.register(block_counter.clone())
		.context("Failed to register block counter metric")?;

	while let Some(z) = rpc::check_connection(&urls).await {
		let (mut write, mut read) = z.split();
		write
			.send(Message::Text(BODY.to_string()))
			.await
			.context("Failed to send ws-message (chain_subscribeFinalizedHeads)")?;

		info!("Connected to Substrate Node");

		struct BlockAvailableMsg {
			params: QueryResult,
			received_at: Instant,
		}

		let (message_tx, message_rx) = sync_channel::<BlockAvailableMsg>(1 << 7);

		tokio::spawn(async move {
			while let Some(message) = read.next().await {
				if let Err(error) = message
					.context("Failed to read web socket message")
					.map(|data| data.into_data())
					.and_then(|data| serde_json::from_slice(&data).context("Fail to decode data"))
					.map(|types::Response { params, .. }| (params.header.number, params))
					.map(|(block_number, params)| {
						(block_number, BlockAvailableMsg {
							params,
							received_at: Instant::now(),
						})
					})
					.and_then(|(block_number, message)| {
						info!(block_number, "Received finalized block header");
						message_tx.send(message).context("Cannot send  message")
					}) {
					error!("Fail to process finalized block header: {error}");
				}
			}
		});

		for message in message_rx {
			if let Some(seconds) = cfg.block_processing_delay {
				let sleep_time = Duration::from_secs(seconds.into());
				let elapsed = message.received_at.elapsed();
				if sleep_time > elapsed {
					tokio::time::sleep(sleep_time - elapsed).await;
				};
			}

			block_counter.inc();
			let header = &message.params.header;

			// now this is in `u64`
			let block_number = header.number;
			info!(
				block_number,
				"Processing finalized block (delayed for {} seconds)",
				message.received_at.elapsed().as_secs()
			);

			let begin = SystemTime::now();

			// TODO: Setting max rows * 2 to match extended matrix dimensions
			let max_rows = header.extrinsics_root.rows * 2;
			let max_cols = header.extrinsics_root.cols;
			if max_cols < 3 {
				error!(block_number, "chunk size less than 3");
			}
			let commitment = header.extrinsics_root.commitment.clone();

			let cell_count = rpc::cell_count_for_confidence(cfg.confidence);
			let positions = rpc::generate_random_cells(max_rows, max_cols, cell_count);
			info!(block_number, "Random cells generated: {}", positions.len());

			let (ipfs_fetched, unfetched) = fetch_cells_from_dht(
				&ipfs,
				block_number,
				&positions,
				cfg.max_parallel_fetch_tasks,
			)
			.await
			.context("Failed to fetch cells from DHT")?;

			info!(
				block_number,
				"Number of cells fetched from DHT: {}",
				ipfs_fetched.len()
			);

			let mut rpc_fetched = if cfg.disable_rpc {
				vec![]
			} else {
				rpc::get_kate_proof(&rpc_url, block_number, unfetched)
					.await
					.context("Failed to fetch cells from node RPC")?
			};

			info!(
				block_number,
				"Number of cells fetched from RPC: {}",
				rpc_fetched.len()
			);

			let mut cells = vec![];
			cells.extend(ipfs_fetched);
			cells.extend(rpc_fetched.clone());

			if positions.len() > cells.len() {
				error!(
					block_number,
					"Failed to fetch {} cells",
					positions.len() - cells.len()
				);
				continue;
			}

			let count = proof::verify_proof(
				block_number,
				max_rows,
				max_cols,
				&cells,
				commitment.clone(),
				pp.clone(),
			);
			info!(
				block_number,
				"Completed {count} verification rounds in \t{:?}",
				begin.elapsed()?
			);

			// write confidence factor into on-disk database
			store_confidence_in_db(db.clone(), block_number, count as u32)
				.context("Failed to store confidence in DB")?;

			let conf = calculate_confidence(count as u32);
			info!(block_number, "Confidence factor: {}", conf);

			// push latest mined block's header into column family specified
			// for keeping block headers, to be used
			// later for verifying IPFS stored data
			//
			// @note this same data store is also written to in
			// another competing thread, which syncs all block headers
			// in range [0, LATEST], where LATEST = latest block number
			// when this process started
			store_block_header_in_db(db.clone(), block_number, header)
				.context("Failed to store block header in DB")?;

			if let Some(partition) = &cfg.block_matrix_partition {
				let positions = rpc::generate_partition_cells(partition, max_rows, max_cols);
				info!(
					block_number,
					"Fetching partition ({}/{}) from RPC", partition.number, partition.fraction
				);
				for cells in positions.chunks(30) {
					let partition_fetched =
						rpc::get_kate_proof(&rpc_url, block_number, cells.to_vec())
							.await
							.context("Failed to fetch cells from node RPC")?;
					let partition_fetched_filtered = partition_fetched
						.into_iter()
						.filter(|cell| {
							!rpc_fetched
								.iter()
								.any(move |rpc_cell| rpc_cell.position.eq(&cell.position))
						})
						.collect::<Vec<_>>();
					rpc_fetched.extend(partition_fetched_filtered.clone());
				}
			}

			let rpc_fetched_len = rpc_fetched.len();
			insert_into_dht(
				&ipfs,
				block_number,
				rpc_fetched,
				cfg.max_parallel_fetch_tasks,
			)
			.await;
			info!(block_number, "{rpc_fetched_len} cells inserted into DHT");

			// notify ipfs-based application client
			// that newly mined block has been received
			block_tx
				.send(types::ClientMsg::from(message.params.header))
				.context("Failed to send block message")?;
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::rpc::cell_count_for_confidence;

	#[test]
	fn test_cell_count_for_confidence() {
		let count = 1;
		assert!(cell_count_for_confidence(60f64) > count);
		assert_eq!(
			cell_count_for_confidence(100f64),
			(-((1f64 - (99f64 / 100f64)).log2())).ceil() as u32
		);
		assert_eq!(
			cell_count_for_confidence(49f64),
			(-((1f64 - (99f64 / 100f64)).log2())).ceil() as u32
		);
		assert!(
			(cell_count_for_confidence(99.99999999)) < 10
				&& (cell_count_for_confidence(99.99999999)) > 0
		);
	}
}
