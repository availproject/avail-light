extern crate confy;
extern crate rocksdb;
extern crate structopt;

use std::{
	sync::{mpsc::sync_channel, Arc},
	thread,
	time::SystemTime,
};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use ipfs_embed::{Multiaddr, PeerId};
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

	let parsed_log_level = cfg.log_level.to_uppercase().parse::<log::LevelFilter>();

	SimpleLogger::new()
		.with_level(*parsed_log_level.as_ref().unwrap_or(&log::LevelFilter::Info))
		.init()?;

	if let Err(parse_error) = parsed_log_level {
		log::warn!("Using default log level: {}", parse_error);
	}

	log::info!("Using {:?}", cfg);

	// Prepare key value data store opening
	//
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

	// this one will spawn one thread for running ipfs client, while managing data discovery
	// and reconstruction
	let db_1 = db.clone();
	let cfg_ = cfg.clone();
	thread::spawn(move || {
		client::run_client(
			cfg_,
			db_1,
			block_rx,
			self_info_tx,
			destroy_rx,
			cell_query_rx,
		)
		.unwrap();
	});
	if let Ok((peer_id, addrs)) = self_info_rx.recv() {
		log::info!("IPFS backed application client: {}\t{:?}", peer_id, addrs);
	};
	let rpc_url = match rpc::check_http(cfg.full_node_rpc).await {
		Ok(a) => a,
		Err(e) => return Err(e),
	};
	let rpc_: &str = &*rpc_url;
	let block_header = rpc::get_chain_header(rpc_).await?;

	let latest_block = hex_to_u64_block_number(block_header.number);
	let rpc_ = rpc_url.clone();
	let db_2 = db.clone();
	let app_id: u32 = cfg.app_id as u32;
	tokio::spawn(async move {
		sync::sync_block_headers(rpc_.clone(), 0, latest_block, db_2, app_id).await;
	});

	log::info!("Syncing block headers from 0 to {}", latest_block);
	//@TODO: better option than loop needed 
	loop{
	let ws_ = rpc::check_connection(cfg.full_node_ws.clone()).await;
	let ws = match ws_ {
		Ok(a) => a,
		Err(e) => return Err(e),
	};
		match ws {
			Some(a) => {
				let (mut write, mut read) = a.split();
				write
					.send(Message::Text(
						r#"{"id":1, "jsonrpc":"2.0", "method": "subscribe_newHead"}"#.to_string()
							+ "\n",
					))
					.await
					.context("ws-message(subscribe_newHead) send failed")?;

				// let _subscription_result = read.next().await.unwrap().unwrap().into_data();
				let s = read.next().await;			
				log::info!("Connected to Substrate Node");

				let db_3 = db.clone();
				let cf_handle_0 = db_3
					.cf_handle(consts::CONFIDENCE_FACTOR_CF)
					.context("failed to get cf handle")?;
				let cf_handle_1 = db_3
					.cf_handle(consts::BLOCK_HEADER_CF)
					.context("failed to get cf handle")?;

					// while(true) {
					// 	match(read.next().await) {
					// 		Ok(_) => {

					// 		}
					// 		Err(_) => {}
					// 	};
					// }
					
					
					while let Some(message) = read.next().await {
							let data = message?.into_data();
							match serde_json::from_slice(&data) {
								Ok(response) => {
								let resp: types::Response = response;
								let header = resp.params.result;

								// well this is in hex form as `String`
								let block_num_hex = header.number.clone();
								// now this is in `u64`
								let num = hex_to_u64_block_number(block_num_hex);

								let begin = SystemTime::now();

								let max_rows = header.extrinsics_root.rows;
								let max_cols = header.extrinsics_root.cols;
								if max_cols < 3 {
									log::error!("chunk size less than 3");
								}
								let commitment = header.extrinsics_root.commitment.clone();
								//hyper request for getting the kate query request
								let cells =
									rpc::get_kate_proof(&rpc_url, num, max_rows, max_cols, app_id).await?;
								//hyper request for verifying the proof
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
								db_3.put_cf(cf_handle_0, num.to_be_bytes(), count.to_be_bytes())
									.context("failed to write confidence factor")?;

								let conf = calculate_confidence(count);
								let app_index = header.app_data_lookup.index.clone();

								/*note:
								The following is the part when the user have already subscribed
								to an appID and now its verifying every cell that contains the data
								*/
								if !app_index.is_empty() {
									let req_id = cfg.app_id as u32;
									let req_conf = cfg.confidence;
									for i in 0..app_index.len() {
										if req_id == app_index[i].0 {
											if conf >= req_conf && req_id > 0 {
												let req_cells = match rpc::get_kate_proof(
													&rpc_url, num, max_rows, max_cols, req_id,
												)
												.await
												{
													Ok(req_cells) => Some(req_cells),
													Err(_) => None,
												};
												match req_cells {
														Some(req_cells) => {
										log::info!("\n💡Verifying all {} cells containing data of block :{} because app id {} is given ", req_cells.len(), num, req_id);
										//hyper request for verifying the proof
															let count =Some(proof::verify_proof(num, max_rows, max_cols, req_cells, commitment.clone()));
															if let Some(j) = count {
																log::info!(
																			"✅ Completed {} rounds of verification for block number {} ",
																			j, num
																			);
															}else{
																log::info!("\n ❌proof verification failed, data availability cannot ensured");
															}
														}
														_ => log::info!("\n ❌ getting proof cells failed, data availability cannot be ensured"),
													}
											}
										} else {
											continue;
										}
									}
								}

								// push latest mined block's header into column family specified
								// for keeping block headers, to be used
								// later for verifying IPFS stored data
								//
								// @note this same data store is also written to in
								// another competing thread, which syncs all block headers
								// in range [0, LATEST], where LATEST = latest block number
								// when this process started
								db_3.put_cf(
									cf_handle_1,
									num.to_be_bytes(),
									serde_json::to_string(&header)?.as_bytes(),
								)
								.context("failed to write block header")?;

								// notify ipfs-based application client
								// that newly mined block has been received
								block_tx
									.send(types::ClientMsg {
										num,
										max_rows,
										max_cols,
										header,
									})
									.context("failed to send block to client")?;
							},
							Err(error) => log::info!("Misconstructed Header: {:?}", error),
							}
						}
			},
			_ => {

				},
		};
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

/* note:
	following are the support functions.
*/
pub fn fill_cells_with_proofs(cells: &mut Vec<types::Cell>, proof: &types::BlockProofResponse) {
	assert_eq!(80 * cells.len(), proof.result.len());
	for i in 0..cells.len() {
		let mut v = Vec::new();
		v.extend_from_slice(&proof.result[i * 80..(i + 1) * 80]);
		cells[i].proof = v;
	}
}

fn hex_to_u64_block_number(num: String) -> u64 {
	let wo_prefix = num.trim_start_matches("0x");
	u64::from_str_radix(wo_prefix, 16).unwrap()
}
