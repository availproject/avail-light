//! Light client for data availability sampling and verification.
//!
//! Sampling and verification are prerequisites for application client, so [`run`] function should be on main thread and exit in case of failures.
//!
//! # Flow
//!
//! * Connect to the Avail node WebSocket stream and start listening to finalized headers
//! * Generate random cells for random data sampling (8 cells currently)
//! * Retrieve cell proofs from a) DHT and/or b) via RPC call from the node, in that order
//! * Verify proof using the received cells
//! * Calculate block confidence and store it in RocksDB
//! * Insert cells to to DHT for remote fetch
//! * Notify the consumer (app client) a new block has been verified
//!
//! # Notes
//!
//! In case delay is configured, block processing is delayed for configured time.  
//! In case RPC is disabled, RPC calls will be skipped.  
//! In case partition is configured, block partition is fetched and inserted into DHT.

use std::{
	sync::{
		mpsc::{sync_channel, SyncSender},
		Arc, Mutex,
	},
	time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result};
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use futures::future::join_all;
use futures_util::{SinkExt, StreamExt};

use prometheus::{Counter, Gauge, Registry};
use rocksdb::DB;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{error, info};

use crate::{
	data::{
		fetch_cells_from_dht, insert_into_dht, store_block_header_in_db, store_confidence_in_db,
	},
	http::calculate_confidence,
	network::NetworkService,
	prometheus_handler, proof, rpc,
	types::{self, ClientMsg, LightClientConfig, QueryResult},
};

/// Defines prometheus metrics for light client.
struct LCMetrics {
	session_block_counter: Counter,
	total_block_number: Gauge,
	dht_fetched: Gauge,
	node_rpc_fetched: Gauge,
	block_confidence: Gauge,
	rpc_call_duration: Gauge,
	dht_put_duration: Gauge,
}

impl LCMetrics {
	fn register(registry: &Registry) -> Result<Self> {
		Ok(Self {
			session_block_counter: prometheus_handler::register(
				prometheus::Counter::new(
					"session_block_number",
					"Number of blocks processed by the light client since (re)start",
				)?,
				registry,
			)?,
			total_block_number: prometheus_handler::register(
				prometheus::Gauge::new(
					"total_block_number",
					"Current block number (as received from Avail header)",
				)?,
				registry,
			)?,
			dht_fetched: prometheus_handler::register(
				prometheus::Gauge::new("dht_fetched", "Number of cells fetched from DHT")?,
				registry,
			)?,
			node_rpc_fetched: prometheus_handler::register(
				prometheus::Gauge::new(
					"node_rpc_fetched",
					"Number of cells fetched via RPC call to node",
				)?,
				registry,
			)?,
			block_confidence: prometheus_handler::register(
				prometheus::Gauge::new("block_confidence", "Block confidence of current block")?,
				registry,
			)?,
			rpc_call_duration: prometheus_handler::register(
				prometheus::Gauge::new(
					"rpc_call_duration",
					"Time needed to retrieve all cells via RPC for current block (in seconds)",
				)?,
				registry,
			)?,
			dht_put_duration: prometheus_handler::register(
				prometheus::Gauge::new(
					"dht_put_duration",
					"Time needed to perform DHT PUT operation for current block (in seconds",
				)?,
				registry,
			)?,
		})
	}
}

/// Runs light client.
///
/// # Arguments
///
/// * `cfg` - Light client configuration
/// * `db` - Database to store confidence and block header
/// * `swarm` - Reference to Libp2p swarm component
/// * `rpc_url` - Node's RPC URL for fetching data unavailable in DHT (if configured)
/// * `block_tx` - Channel used to send header of verified block
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
/// * `registry` - Prometheus metrics registry
/// * `counter` - Processed block mutex counter
pub async fn run(
	cfg: LightClientConfig,
	db: Arc<DB>,
	net_svc: NetworkService,
	rpc_url: String,
	block_tx: Option<SyncSender<ClientMsg>>,
	pp: PublicParameters,
	registry: Registry,
	counter: Arc<Mutex<u64>>,
) -> Result<()> {
	info!("Starting light client...");
	const BODY: &str = r#"{"id":1, "jsonrpc":"2.0", "method": "chain_subscribeFinalizedHeads"}"#;
	let urls = rpc::parse_urls(&cfg.full_node_ws)?;
	// Register metrics
	let metrics = LCMetrics::register(&registry)?;

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
						(
							block_number,
							BlockAvailableMsg {
								params,
								received_at: Instant::now(),
							},
						)
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

			metrics.session_block_counter.inc();
			metrics
				.total_block_number
				.set(message.params.header.number as f64);
			let header = &message.params.header;

			// now this is in `u64`
			let block_number = header.number;
			info!(
				block_number,
				"block_delay" = message.received_at.elapsed().as_secs(),
				"Processing finalized block (delayed for {} seconds)",
				message.received_at.elapsed().as_secs(),
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
			info!(
				block_number,
				"cells_requested" = positions.len(),
				"Random cells generated: {}",
				positions.len()
			);

			let (cells_fetched, unfetched) = fetch_cells_from_dht(
				&net_svc,
				block_number,
				&positions,
				cfg.dht_parallelization_limit,
			)
			.await
			.context("Failed to fetch cells from DHT")?;

			info!(
				block_number,
				"cells_from_dht" = cells_fetched.len(),
				"Number of cells fetched from DHT: {}",
				cells_fetched.len()
			);
			metrics.dht_fetched.set(cells_fetched.len() as f64);

			let mut rpc_fetched = if cfg.disable_rpc {
				vec![]
			} else {
				rpc::get_kate_proof(&rpc_url, block_number, unfetched)
					.await
					.context("Failed to fetch cells from node RPC")?
			};

			info!(
				block_number,
				"cells_from_rpc" = rpc_fetched.len(),
				"Number of cells fetched from RPC: {}",
				rpc_fetched.len()
			);
			metrics.node_rpc_fetched.set(rpc_fetched.len() as f64);

			let mut cells = vec![];
			cells.extend(cells_fetched);
			cells.extend(rpc_fetched.clone());

			if positions.len() > cells.len() {
				error!(
					block_number,
					"Failed to fetch {} cells",
					positions.len() - cells.len()
				);
				continue;
			}

			if !cfg.disable_proof_verification {
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
				let mut lock = counter.lock().unwrap();
				*lock = block_number;

				let conf = calculate_confidence(count as u32);
				info!(
					block_number,
					"confidence" = conf,
					"Confidence factor: {}",
					conf
				);
				metrics.block_confidence.set(conf);
			}

			// push latest mined block's header into column family specified
			// for keeping block headers, to be used
			// later for verifying DHT stored data
			//
			// @note this same data store is also written to in
			// another competing thread, which syncs all block headers
			// in range [0, LATEST], where LATEST = latest block number
			// when this process started
			store_block_header_in_db(db.clone(), block_number, header)
				.context("Failed to store block header in DB")?;

			let mut begin = SystemTime::now();
			if let Some(partition) = &cfg.block_matrix_partition {
				let positions = rpc::generate_partition_cells(partition, max_rows, max_cols);
				info!(
					block_number,
					"partition_cells_requested" = positions.len(),
					"Fetching partition ({}/{}) from RPC",
					partition.number,
					partition.fraction
				);

				let rpc_cells = positions.chunks(cfg.max_cells_per_rpc).collect::<Vec<_>>();
				for batch in rpc_cells
					.chunks(cfg.query_proof_rpc_parallel_tasks)
					.map(|e| {
						join_all(
							e.iter()
								.map(|n| rpc::get_kate_proof(&rpc_url, block_number, n.to_vec()))
								.collect::<Vec<_>>(),
						)
					}) {
					for partition_fetched in batch
						.await
						.into_iter()
						.enumerate()
						.map(|(i, e)| {
							e.context(format!("Failed to fetch cells from node RPC at batch {i}"))
						})
						.collect::<Vec<_>>()
					{
						let partition_fetched_filtered = partition_fetched?
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
			}
			let partition_time_elapsed = begin.elapsed()?;
			let rpc_fetched_len = rpc_fetched.len();
			info!(
				block_number,
				"partition_retrieve_time_elapsed" = partition_time_elapsed.as_secs_f64(),
				"partition_cells_fetched" = rpc_fetched_len,
				"Partition cells received. Time elapsed: \t{:?}",
				partition_time_elapsed
			);
			metrics
				.rpc_call_duration
				.set(partition_time_elapsed.as_secs_f64());

			begin = SystemTime::now();

			insert_into_dht(
				&net_svc,
				block_number,
				rpc_fetched,
				cfg.dht_parallelization_limit,
				cfg.ttl,
			)
			.await;

			let dht_put_time_elapsed = begin.elapsed()?;
			info!(
				block_number,
				"partition_dht_insert_time_elapsed" = dht_put_time_elapsed.as_secs_f64(),
				"{rpc_fetched_len} cells inserted into DHT. Time elapsed: \t{:?}",
				dht_put_time_elapsed
			);
			metrics
				.dht_put_duration
				.set(dht_put_time_elapsed.as_secs_f64());

			// notify dht-based application client
			// that newly mined block has been received
			if let Some(ref channel) = block_tx {
				channel
					.send(types::ClientMsg::from(message.params.header))
					.context("Failed to send block message")?;
			}
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
