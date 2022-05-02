#![feature(result_option_inspect, iterator_try_collect)]

use std::{
	path::Path,
	sync::{
		mpsc::{sync_channel, Receiver, SyncSender},
		Arc,
	},
	thread,
	time::{Duration, SystemTime},
};

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use ipfs_embed::{Multiaddr, PeerId};
use rocksdb::{ColumnFamilyDescriptor, Options, DB};
use serde::de::DeserializeOwned;
use simple_logger::SimpleLogger;
use structopt::StructOpt;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::{
	consts::{BLOCK_CID_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF},
	http::calculate_confidence,
	types::{
		CellContentQueryPayload, ClientMsg, Header, Response, RuntimeConfig, SubscriptionResponse,
	},
};

mod client;
mod consts;
mod data;
mod error;
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
	let cfg: RuntimeConfig = confy::load_path(opts.config)?;

	// Logger & DB
	setup_logger(&cfg);
	let db = setup_db(cfg.avail_path.clone())?;

	// HTTP Server.
	let cell_query_rx = spawn_http_server(Arc::clone(&db), cfg.clone());

	// Spawns IPFS client & data discovery, and header synchronizer.
	let (destroy_tx, block_tx) = spawn_client(Arc::clone(&db), cfg.clone(), cell_query_rx);
	let rpc_url = rpc::check_http(&cfg.full_node_rpc).await?;
	let _ = spawn_block_headers_synchronizer(Arc::clone(&db), &cfg, rpc_url.clone())
		.await
		.expect("Block headers synchronizer task cannot be initialize");

	// Susbribe to new headers
	subscribe_new_head(db, cfg, &rpc_url, destroy_tx, block_tx).await
}

fn ws_message_into<T>(ws_message: Message) -> Result<T>
where
	T: DeserializeOwned,
{
	let msg = ws_message.into_text()?;
	let o = serde_json::from_str::<T>(&msg)
		.map_err(|e| anyhow!("Invalid expected Message. Error: {}, Payload: {:?}", e, msg))?;
	Ok(o)
}

async fn subscribe_new_head(
	db: Arc<DB>,
	cfg: RuntimeConfig,
	rpc_url: &str,
	destroy_tx: SyncSender<bool>,
	block_tx: SyncSender<ClientMsg>,
) -> Result<()> {
	const BODY: &str = r#"{"id":1, "jsonrpc":"2.0", "method": "chain_subscribeFinalizedHeads"}"#;
	let urls = rpc::parse_urls(&cfg.full_node_ws)?;

	while let Some(z) = rpc::check_connection(&urls).await {
		let (mut write, mut read) = z.split();
		write
			.send(Message::Text(BODY.to_string()))
			.await
			.context("ws-message(subscribe_newHead) send failed")?;

		// Read the subscription id:
		let id = read
			.next()
			.await
			.ok_or_else(|| anyhow!("Finalized heads subscription failed"))?
			.map(ws_message_into::<SubscriptionResponse>)??
			.subscription_id;
		log::info!("Subscrition to FinalizedHeads with id `{}`", id);

		// Read all new finalized heads.
		while let Some(message) = read.next().await {
			match ws_message_into::<Response>(message?) {
				Ok(response) => {
					let header = response.params.result;
					if header.number != 0 {
						let client_msg = new_header_received(&db, &cfg, rpc_url, header).await?;
						// notify ipfs-based application client
						// that newly mined block has been received
						block_tx
							.send(client_msg)
							.context("failed to send block to client")?;
					}
				},
				Err(e) => log::error!("Misconstructed Header: {:?}", e),
			}
		}

		// Retry connections
		sleep(Duration::from_secs(5)).await;
	}
	// inform ipfs-backed application client running thread
	// that it can kill self now, as process is going to die itself !
	destroy_tx
		.send(true)
		.context("failed to send block to client")?;

	Ok(())
}

async fn new_header_received(
	db: &DB,
	cfg: &RuntimeConfig,
	rpc_url: &str,
	header: Header,
) -> Result<ClientMsg> {
	let confidence = sync::confidence_cf(db);

	// well this is in hex form as `String`
	// now this is in `u64`
	let num = header.number;
	let num_key = num.to_be_bytes();

	let begin = SystemTime::now();

	// TODO: Setting max rows * 2 to match extended matrix dimensions
	let max_rows = header.extrinsics_root.rows * 2;
	let max_cols = header.extrinsics_root.cols;
	if max_cols < 3 {
		log::error!("chunk size less than 3: header = {:?}", header);
	}
	let commitment = header.extrinsics_root.commitment.clone();

	//hyper request for getting the kate query request
	let cells = rpc::get_kate_proof(rpc_url, num, max_rows, max_cols, cfg.app_id as u32).await?;
	//hyper request for verifying the proof
	let count = proof::verify_proof(num, max_rows, max_cols, cells.clone(), commitment.clone());
	log::info!(
		"Completed {} verification rounds for block {}\t{:?}",
		count,
		num,
		begin.elapsed().unwrap_or_default()
	);

	// write confidence factor into on-disk database
	db.put_cf(&confidence, num_key, count.to_be_bytes())
		.context("failed to write confidence factor")?;

	let conf = calculate_confidence(count);
	let app_index = header.app_data_lookup.index.clone();

	/*note:
	The following is the part when the user have already subscribed
	to an appID and now its verifying every cell that contains the data
	*/
	if cfg.app_id > 0 && conf >= cfg.confidence && !app_index.is_empty() {
		for (app_id, _) in app_index.iter().filter(|app| cfg.app_id as u32 == app.0) {
			let proof = rpc::get_kate_proof(rpc_url, num, max_rows, max_cols, *app_id).await;
			if let Ok(req_cells) = proof {
				log::info!("\n💡Verifying all {} cells containing data of block :{} because app id {} is given ", req_cells.len(), num, app_id);
				//hyper request for verifying the proof
				let count =
					proof::verify_proof(num, max_rows, max_cols, req_cells, commitment.clone());
				log::info!(
					"✅ Completed {} rounds of verification for block number {} ",
					count,
					num
				);
			} else {
				log::info!("\n ❌ getting proof cells failed, data availability cannot be ensured");
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
	let block_header = sync::header_cf(db);
	let raw_header = serde_json::to_string(&header)?.into_bytes();
	db.put_cf(&block_header, num_key, raw_header)
		.context("failed to write block header")?;

	Ok(ClientMsg {
		num,
		max_rows,
		max_cols,
		header,
	})
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
pub fn fill_cells_with_proofs(cells: &mut [types::Cell], proof: &types::BlockProofResponse) {
	assert_eq!(80 * cells.len(), proof.result.len());
	for (i, cell) in cells.iter_mut().enumerate() {
		cell.proof = proof.result[i * 80..(i + 1) * 80].to_vec();
	}
}

/// Setup the logger.
fn setup_logger(cfg: &RuntimeConfig) {
	let parsed_log_level = cfg.log_level.to_uppercase().parse::<log::LevelFilter>();

	SimpleLogger::new()
		.with_level(*parsed_log_level.as_ref().unwrap_or(&log::LevelFilter::Info))
		.init()
		.expect("Logger cannot be initialize");

	if let Err(parse_error) = parsed_log_level {
		log::warn!("Using default log level: {}", parse_error);
	}
	log::info!("Using {:?}", cfg);
}

/// Setup DB.
fn setup_db<P: AsRef<Path>>(path: P) -> Result<Arc<DB>> {
	// Prepare key value data store opening
	// cf = column family
	let mut opts = Options::default();
	opts.set_max_write_buffer_number(16);

	let cf_list = [CONFIDENCE_FACTOR_CF, BLOCK_HEADER_CF, BLOCK_CID_CF]
		.into_iter()
		.map(|name| ColumnFamilyDescriptor::new(name, opts.clone()))
		.collect::<Vec<_>>();

	// DB opts
	opts.create_if_missing(true);
	opts.create_missing_column_families(true);

	let db = DB::open_cf_descriptors(&opts, path, cf_list).context("Failed to open database")?;

	Ok(Arc::new(db))
}

/// Spawns one thread of execution which runs one http server
/// for handling RPC
fn spawn_http_server(db: Arc<DB>, cfg: RuntimeConfig) -> Receiver<CellContentQueryPayload> {
	// Have access to key value data store, now this can be safely used
	// from multiple threads of execution

	// This channel will be used for message based communication between
	// two tasks
	//
	// task_0: HTTP request handler ( query sender )
	// task_1: IPFS client ( query receiver & hopefully successfully resolver )
	let (cell_query_tx, cell_query_rx) = sync_channel::<CellContentQueryPayload>(32);

	thread::spawn(move || {
		http::run_server(db, cfg, cell_query_tx).unwrap();
	});

	cell_query_rx
}

/// Spawns one thread for running ipfs client, while managing data discovery
// and reconstruction
fn spawn_client(
	db: Arc<DB>,
	cfg: RuntimeConfig,
	cell_query_rx: Receiver<CellContentQueryPayload>,
) -> (SyncSender<bool>, SyncSender<ClientMsg>) {
	// communication channels being established for talking to
	// ipfs backed application client
	let (block_tx, block_rx) = sync_channel::<ClientMsg>(128);
	let (self_info_tx, self_info_rx) = sync_channel::<(PeerId, Multiaddr)>(1);
	let (destroy_tx, destroy_rx) = sync_channel::<bool>(1);

	thread::spawn(move || {
		client::run_client(cfg, db, block_rx, self_info_tx, destroy_rx, cell_query_rx).unwrap();
	});
	if let Ok((peer_id, addrs)) = self_info_rx.recv() {
		log::info!("IPFS backed application client: {}\t{:?}", peer_id, addrs);
	};

	(destroy_tx, block_tx)
}

async fn spawn_block_headers_synchronizer(
	db: Arc<DB>,
	cfg: &RuntimeConfig,
	rpc_url: String,
) -> Result<()> {
	let block_header = rpc::get_chain_header(&rpc_url).await?;
	let latest_block = block_header.number;
	if latest_block > 0 {
		log::info!("Syncing block headers from 1 to {}", latest_block);

		let app_id = cfg.app_id as u32;
		tokio::spawn(async move {
			sync::sync_block_headers(&rpc_url, 1, latest_block, db, app_id).await;
		});
	}
	Ok(())
}
