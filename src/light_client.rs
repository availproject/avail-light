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
	sync::{Arc, Mutex},
	time::{Instant, SystemTime},
};

use anyhow::{Context, Result};
use avail_subxt::{
	api::runtime_types::da_primitives::header::extension::HeaderExtension, primitives::Header,
	AvailConfig,
};
use codec::Encode;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use futures::future::join_all;
use kate_recovery::{
	commitments, data,
	matrix::{Dimensions, Position},
};
use rocksdb::DB;
use sp_core::blake2_256;
use subxt::{utils::H256, OnlineClient};
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{error, info};

use crate::{
	data::{store_block_header_in_db, store_confidence_in_db},
	http::calculate_confidence,
	network::Client,
	proof, rpc,
	telemetry::metrics::{MetricEvent, Metrics},
	types::{self, BlockVerified, LightClientConfig},
};

pub async fn process_block(
	cfg: &LightClientConfig,
	db: Arc<DB>,
	network_client: &Client,
	rpc_client: &OnlineClient<AvailConfig>,
	pp: &PublicParameters,
	header: &Header,
	received_at: Instant,
	metrics: &Metrics,
	counter: Arc<Mutex<u32>>,
) -> Result<()> {
	metrics.record(MetricEvent::SessionBlockCounter);
	metrics.record(MetricEvent::TotalBlockNumber(header.number));

	let block_number = header.number;
	let header_hash: H256 = Encode::using_encoded(header, blake2_256).into();

	info!(
		{ block_number, block_delay = received_at.elapsed().as_secs()},
		"Processing finalized block",
	);

	let begin = SystemTime::now();

	let HeaderExtension::V1(xt) = &header.extension;
	let Some(dimensions) = Dimensions::new(xt.commitment.rows, xt.commitment.cols) else {
			    info!(
				    block_number,
				    "Skipping block with invalid dimensions {}x{}",
				    xt.commitment.rows,
				    xt.commitment.cols
			    );
	    return Ok(());
	};

	if dimensions.cols() <= 2 {
		error!(block_number, "more than 2 columns is required");
		return Ok(());
	}

	let commitments = commitments::from_slice(&xt.commitment.commitment)?;

	let cell_count = rpc::cell_count_for_confidence(cfg.confidence);
	let positions = rpc::generate_random_cells(&dimensions, cell_count);
	info!(
		block_number,
		"cells_requested" = positions.len(),
		"Random cells generated: {}",
		positions.len()
	);

	let (cells_fetched, unfetched) = network_client
		.fetch_cells_from_dht(block_number, &positions)
		.await;

	info!(
		block_number,
		"cells_from_dht" = cells_fetched.len(),
		"Number of cells fetched from DHT: {}",
		cells_fetched.len()
	);
	metrics.record(MetricEvent::DHTFetched(cells_fetched.len() as i64));
	metrics.record(MetricEvent::DHTFetchedPercentage(
		cells_fetched.len() as f64 / positions.len() as f64,
	));

	let mut rpc_fetched = if cfg.disable_rpc {
		vec![]
	} else {
		rpc::get_kate_proof(rpc_client, header_hash, &unfetched)
			.await
			.context("Failed to fetch cells from node RPC")?
	};

	info!(
		block_number,
		"cells_from_rpc" = rpc_fetched.len(),
		"Number of cells fetched from RPC: {}",
		rpc_fetched.len()
	);
	metrics.record(MetricEvent::NodeRPCFetched(rpc_fetched.len() as i64));

	let mut cells = vec![];
	cells.extend(cells_fetched);
	cells.extend(rpc_fetched.clone());

	if positions.len() > cells.len() {
		error!(
			block_number,
			"Failed to fetch {} cells",
			positions.len() - cells.len()
		);
		return Ok(());
	}

	if !cfg.disable_proof_verification {
		let (verified, unverified) =
			proof::verify(block_number, &dimensions, &cells, &commitments, pp)?;
		let count = verified.len() - unverified.len();
		info!(
			block_number,
			"Completed {count} verification rounds in \t{:?}",
			begin.elapsed()?
		);

		// write confidence factor into on-disk database
		store_confidence_in_db(db.clone(), block_number, verified.len() as u32)
			.context("Failed to store confidence in DB")?;
		let mut lock = counter.lock().unwrap();
		*lock = block_number;

		let conf = calculate_confidence(verified.len() as u32);
		info!(
			block_number,
			"confidence" = conf,
			"Confidence factor: {}",
			conf
		);
		metrics.record(MetricEvent::BlockConfidence(conf));
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
		let positions: Vec<Position> = dimensions
			.iter_extended_partition_positions(partition)
			.collect();
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
						.map(|n| rpc::get_kate_proof(rpc_client, header_hash, n))
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

		let begin = SystemTime::now();

		let rpc_fetched_data_cells = rpc_fetched
			.iter()
			.filter(|cell| !cell.position.is_extended())
			.collect::<Vec<_>>();
		let rpc_fetched_data_rows = data::rows(&dimensions, &rpc_fetched_data_cells);
		let rows_len = rpc_fetched_data_rows.len();

		let dht_insert_rows_success_rate = network_client
			.insert_rows_into_dht(block_number, rpc_fetched_data_rows)
			.await;
		let success_rate: f64 = dht_insert_rows_success_rate.into();
		let time_elapsed = begin.elapsed()?.as_secs_f64();

		info!(
			block_number,
			"DHT PUT rows operation success rate: {dht_insert_rows_success_rate}"
		);

		metrics.record(MetricEvent::DHTPutRowsSuccess(success_rate));

		info!(
			block_number,
			"partition_dht_rows_insert_time_elapsed" = time_elapsed,
			"{rows_len} rows inserted into DHT"
		);

		metrics.record(MetricEvent::DHTPutRowsDuration(time_elapsed));
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
	metrics.record(MetricEvent::RPCCallDuration(
		partition_time_elapsed.as_secs_f64(),
	));

	begin = SystemTime::now();

	let dht_insert_success_rate = network_client
		.insert_cells_into_dht(block_number, rpc_fetched)
		.await;

	info!(
		block_number,
		"DHT PUT operation success rate: {}", dht_insert_success_rate
	);

	metrics.record(MetricEvent::DHTPutSuccess(dht_insert_success_rate as f64));

	let dht_put_time_elapsed = begin.elapsed()?;
	info!(
		block_number,
		"partition_dht_insert_time_elapsed" = dht_put_time_elapsed.as_secs_f64(),
		"{rpc_fetched_len} cells inserted into DHT. Time elapsed: \t{:?}",
		dht_put_time_elapsed
	);

	metrics.record(MetricEvent::DHTPutDuration(
		dht_put_time_elapsed.as_secs_f64(),
	));

	Ok(())
}

/// Runs light client.
///
/// # Arguments
///
/// * `cfg` - Light client configuration
/// * `db` - Database to store confidence and block header
/// * `network_client` - Reference to a libp2p custom network client
/// * `rpc_client` - Node's RPC subxt client for fetching data unavailable in DHT (if configured)
/// * `block_tx` - Channel used to send header of verified block
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
/// * `registry` - Prometheus metrics registry
/// * `counter` - Processed block mutex counter
pub async fn run(
	cfg: LightClientConfig,
	db: Arc<DB>,
	network_client: Client,
	rpc_client: OnlineClient<AvailConfig>,
	block_tx: Option<Sender<BlockVerified>>,
	pp: PublicParameters,
	metrics: Metrics,
	counter: Arc<Mutex<u32>>,
	mut message_rx: Receiver<(Header, Instant)>,
	error_sender: Sender<anyhow::Error>,
) {
	info!("Starting light client...");

	while let Some((header, received_at)) = message_rx.recv().await {
		if let Some(seconds) = cfg.block_processing_delay.sleep_duration(received_at) {
			info!("Sleeping for {seconds:?} seconds");
			tokio::time::sleep(seconds).await;
		}

		if let Err(error) = process_block(
			&cfg,
			db.clone(),
			&network_client,
			&rpc_client,
			&pp,
			&header,
			received_at,
			&metrics,
			counter.clone(),
		)
		.await
		{
			error!("Cannot process block: {error}");
			if let Err(error) = error_sender.send(error).await {
				error!("Cannot send error message: {error}");
			}
			return;
		}

		let Ok(client_msg) = types::BlockVerified::try_from(header) else {
		    error!("Cannot create message from header");
		    continue;
		};

		// notify dht-based application client
		// that newly mined block has been received
		if let Some(ref channel) = block_tx {
			if let Err(error) = channel.send(client_msg).await {
				error!("Cannot send block verified message: {error}");
				continue;
			}
		}
	}
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
