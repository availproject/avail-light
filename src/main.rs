extern crate confy;
extern crate rocksdb;
extern crate structopt;

use std::{
	str::FromStr,
	sync::{mpsc::sync_channel, Arc},
	thread, time,
	time::SystemTime,
};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use ipfs_embed::{Multiaddr, PeerId, Record, Key};
// use kate_recovery::com::{
// 	app_specific_column_cells, reconstruct_app_extrinsics, Cell, ExtendedMatrixDimensions,
// };
use rocksdb::{ColumnFamilyDescriptor, Options, DB};
use simple_logger::SimpleLogger;
use structopt::StructOpt;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::http::calculate_confidence;

mod client;
mod consts;
mod data;
mod http;
mod proof;
mod rpc;
mod sync;
mod types;

#[derive(StructOpt, Debug)]
#[structopt(
	name = "avail-light",
	about = "Light Client for Polygon Avail Blockchain",
	author = "Polygon Avail Team",
	version = "0.1.0"
)]
struct CliOpts {
	#[structopt(
		short = "c",
		long = "config",
		default_value = "config.yaml",
		help = "yaml configuration file"
	)]
	config: String,
}

pub async fn do_main() -> Result<()> {
	let opts = CliOpts::from_args();
	let cfg: types::RuntimeConfig = confy::load_path(opts.config)?;

	let (log_level, parse_error) = cfg
		.log_level
		.to_uppercase()
		.parse::<log::LevelFilter>()
		.map(|log_level| (log_level, None))
		.unwrap_or_else(|parse_err| (log::LevelFilter::Info, Some(parse_err)));

	SimpleLogger::new().with_level(log_level).init()?;

	if let Some(error) = parse_error {
		log::warn!("Using default log level: {}", error);
	}

	log::info!("Using {:?}", cfg);

	// Prepare key value data store opening
	// cf = column family
	let mut confidence_cf_opts = Options::default();
	confidence_cf_opts.set_max_write_buffer_number(16);
	let confidence_cf_desp =
		ColumnFamilyDescriptor::new(consts::CONFIDENCE_FACTOR_CF, confidence_cf_opts);

	let mut block_header_cf_opts = Options::default();
	block_header_cf_opts.set_max_write_buffer_number(16);
	let block_header_cf_desp =
		ColumnFamilyDescriptor::new(consts::BLOCK_HEADER_CF, block_header_cf_opts);

	let mut block_cid_cf_opts = Options::default();
	block_cid_cf_opts.set_max_write_buffer_number(16);
	let block_cid_cf_desp = ColumnFamilyDescriptor::new(consts::BLOCK_CID_CF, block_cid_cf_opts);

	let mut db_opts = Options::default();
	db_opts.create_if_missing(true);
	db_opts.create_missing_column_families(true);

	let db = Arc::new(
		DB::open_cf_descriptors(&db_opts, cfg.avail_path.clone(), vec![
			confidence_cf_desp,
			block_header_cf_desp,
			block_cid_cf_desp,
		])
		.context("Failed to open database")?,
	);
	// Have access to key value data store, now this can be safely used
	// from multiple threads of execution

	// This channel will be used for message based communication between
	// two tasks
	//
	// task_0: HTTP request handler ( query sender )
	// task_1: IPFS client ( query receiver & hopefully successfully resolver )
	let (cell_query_tx, cell_query_rx) =
		sync_channel::<crate::types::CellContentQueryPayload>(1 << 4);

	// this spawns one thread of execution which runs one http server
	// for handling RPC
	let db_0 = db.clone();
	let cfg_ = cfg.clone();
	thread::spawn(move || {
		http::run_server(db_0, cfg_, cell_query_tx).unwrap();
	});

	// communication channels being established for talking to
	// ipfs backed application client
	let (block_tx, block_rx) = sync_channel::<types::ClientMsg>(1 << 7);
	let (self_info_tx, self_info_rx) = sync_channel::<(PeerId, Multiaddr)>(1);
	let (destroy_tx, destroy_rx) = sync_channel::<bool>(1);

	let bootstrap_nodes = &cfg
		.bootstraps
		.iter()
		.map(|(a, b)| (PeerId::from_str(a).expect("Valid peer id"), b.clone()))
		.collect::<Vec<(PeerId, Multiaddr)>>();

	let ipfs = client::make_client(cfg.ipfs_seed, cfg.ipfs_port, &cfg.ipfs_path, bootstrap_nodes).await?;

	#[cfg(feature = "logs")]
	tokio::task::spawn(client::log_events(ipfs.clone()));

	// inform invoker about self
	self_info_tx.send((ipfs.local_peer_id(), ipfs.listeners()[0].clone()))?;

	// this one will spawn one thread for running ipfs client, while managing data discovery
	// and reconstruction
	let db_1 = db.clone();
	let cfg_ = cfg.clone();
	let ipfs_ = ipfs.clone();
	// thread::spawn(move || {
	// 	client::run_client(cfg_, db_1, block_rx, destroy_rx, cell_query_rx, ipfs_).unwrap();
	// });
	if let Ok((peer_id, addrs)) = self_info_rx.recv() {
		log::info!("IPFS backed application client: {}\t{:?}", peer_id, addrs);
	};

	let rpc_url = rpc::check_http(cfg.full_node_rpc).await?.clone();
	let block_header = rpc::get_chain_header(&rpc_url).await?;

	let latest_block = block_header.number;
	let rpc_ = rpc_url.clone();
	let db_2 = db.clone();
	let ipfs2_ = ipfs.clone();

	// TODO: implement proper sync between bootstrap completion and starting the sync function
	thread::sleep(time::Duration::from_secs(3));
	tokio::spawn(async move {
		sync::sync_block_headers(rpc_.clone(), 0, latest_block, db_2, ipfs2_).await;
	});

	const BODY: &str = r#"{"id":1, "jsonrpc":"2.0", "method": "chain_subscribeFinalizedHeads"}"#;
	let urls = rpc::parse_urls(cfg.full_node_ws)?;
	log::info!("Syncing block headers from 0 to {}", latest_block);
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
					let header = params.result;

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
					let mut cells = rpc::generate_random_cells(max_rows, max_cols, num);
					println!(
						"Random cells generated: {} from block: {}",
						cells.len(),
						num
					);

					let ipfs_2 = ipfs.clone();
					// TODO: error handling
					data::ipfs_priority_get_cells(&mut cells, &ipfs_2, &rpc_url, num).await?;

					let count = proof::verify_proof(
						num,
						max_rows,
						max_cols,
						cells.clone(),
						commitment.clone(),
					);
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

					let conf = calculate_confidence(count);
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

					// Push the randomly selected cells to IPFS
					for cell in cells {
						if let Err(error) = ipfs.insert(cell.clone().to_ipfs_block()) {
							log::info!(
								"Error pushing cell to IPFS: {}. Cell reference: {}",
								error,
								cell.reference()
							);
						}
						// Add generated CID to DHT
						if let Err(error) = ipfs
							.put_record(
								Record::new(
									cell.reference().as_bytes().to_vec(),
									cell.cid().to_bytes(),
								),
								ipfs_embed::Quorum::One,
							)
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
						println!("ERRROR FLUSHING: {}", error)
					};

					// notify ipfs-based application client
					// that newly mined block has been received
					// block_tx
					// 	.send(types::ClientMsg {
					// 		num,
					// 		max_rows,
					// 		max_cols,
					// 		header,
					// 	})
					// 	.context("failed to send block to client")?;
				},
				Err(error) => log::info!("Misconstructed Header: {:?}", error),
			}
		}
	}

	// inform ipfs-backed application client running thread
	// that it can kill self now, as process is going to die itself !
	destroy_tx
		.send(true)
		.context("failed to send block to client")?;

	Ok(())
}

#[tokio::main]
pub async fn main() -> Result<()> {
	do_main().await.map_err(|e| {
		log::error!("{:?}", e);
		e
	})
}
