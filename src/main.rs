extern crate confy;
extern crate rocksdb;
extern crate structopt;

use std::{
	str::FromStr,
	sync::{mpsc::sync_channel, Arc},
	thread, time,
};

use anyhow::{Context, Result};
use ipfs_embed::{Multiaddr, PeerId};
use rocksdb::{ColumnFamilyDescriptor, Options, DB};
use simple_logger::SimpleLogger;
use structopt::StructOpt;

use crate::types::Mode;

mod app_client;
mod consts;
mod data;
mod http;
mod light_client;
mod proof;
mod rpc;
mod sync_client;
mod types;
mod util;

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

	let mut app_data_cf_opts = Options::default();
	app_data_cf_opts.set_max_write_buffer_number(16);
	let app_data_cf_desp = ColumnFamilyDescriptor::new(consts::APP_DATA_CF, app_data_cf_opts);

	let mut db_opts = Options::default();
	db_opts.create_if_missing(true);
	db_opts.create_missing_column_families(true);

	let db = Arc::new(
		DB::open_cf_descriptors(&db_opts, cfg.avail_path.clone(), vec![
			confidence_cf_desp,
			block_header_cf_desp,
			block_cid_cf_desp,
			app_data_cf_desp,
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
	let (cell_query_tx, _) = sync_channel::<crate::types::CellContentQueryPayload>(1 << 4);

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
	let (destroy_tx, _) = sync_channel::<bool>(1);

	let bootstrap_nodes = &cfg
		.bootstraps
		.iter()
		.map(|(a, b)| (PeerId::from_str(a).expect("Valid peer id"), b.clone()))
		.collect::<Vec<(PeerId, Multiaddr)>>();

	let ipfs = data::init_ipfs(
		cfg.ipfs_seed,
		cfg.ipfs_port,
		&cfg.ipfs_path,
		bootstrap_nodes,
	)
	.await?;

	tokio::task::spawn(data::log_ipfs_events(ipfs.clone()));

	// inform invoker about self
	self_info_tx.send((ipfs.local_peer_id(), ipfs.listeners()[0].clone()))?;

	if let Ok((peer_id, addrs)) = self_info_rx.recv() {
		log::info!("IPFS backed application client: {}\t{:?}", peer_id, addrs);
	};

	let rpc_url = rpc::check_http(cfg.full_node_rpc.clone()).await?.clone();

	if let Mode::AppClient(app_id) = Mode::from(cfg.app_id) {
		tokio::task::spawn(app_client::run(
			ipfs.clone(),
			db.clone(),
			rpc_url.clone(),
			app_id,
			block_rx,
			cfg.max_parallel_fetch_tasks,
		));
	}

	let block_header = rpc::get_chain_header(&rpc_url).await?;
	let latest_block = block_header.number;

	// TODO: implement proper sync between bootstrap completion and starting the sync function
	thread::sleep(time::Duration::from_secs(3));

	tokio::task::spawn(sync_client::run(
		rpc_url.clone(),
		0,
		latest_block,
		db.clone(),
		ipfs.clone(),
		cfg.max_parallel_fetch_tasks,
	));

	if let Err(error) = light_client::run(
		cfg.full_node_ws,
		db,
		ipfs,
		rpc_url,
		block_tx,
		cfg.max_parallel_fetch_tasks,
	)
	.await
	{
		log::info!("Error running light client: {}", error);
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
