#![doc = include_str!("../README.md")]

use std::{
	net::{IpAddr, Ipv4Addr, SocketAddr},
	str::FromStr,
	sync::{mpsc::sync_channel, Arc, Mutex},
	thread, time,
};

use ::prometheus::Registry;
use anyhow::{Context, Result};
use ipfs_embed::{Multiaddr, PeerId};
use rand::{thread_rng, Rng};
use rocksdb::{ColumnFamilyDescriptor, Options, DB};
use structopt::StructOpt;
use tracing::{error, info, metadata::ParseLevelError, trace, warn, Level};
use tracing_subscriber::{
	fmt::format::{self, DefaultFields, Format, Full, Json},
	FmtSubscriber,
};

use crate::{
	consts::{APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF},
	types::{Mode, RuntimeConfig},
};

mod app_client;
mod consts;
mod data;
mod http;
mod light_client;
mod prometheus_handler;
mod proof;
mod rpc;
mod sync_client;
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

fn init_db(path: &str) -> Result<Arc<DB>> {
	let mut confidence_cf_opts = Options::default();
	confidence_cf_opts.set_max_write_buffer_number(16);

	let mut block_header_cf_opts = Options::default();
	block_header_cf_opts.set_max_write_buffer_number(16);

	let mut app_data_cf_opts = Options::default();
	app_data_cf_opts.set_max_write_buffer_number(16);

	let cf_opts = vec![
		ColumnFamilyDescriptor::new(CONFIDENCE_FACTOR_CF, confidence_cf_opts),
		ColumnFamilyDescriptor::new(BLOCK_HEADER_CF, block_header_cf_opts),
		ColumnFamilyDescriptor::new(APP_DATA_CF, app_data_cf_opts),
	];

	let mut db_opts = Options::default();
	db_opts.create_if_missing(true);
	db_opts.create_missing_column_families(true);

	let db = DB::open_cf_descriptors(&db_opts, &path, cf_opts)?;
	Ok(Arc::new(db))
}

fn json_subscriber(log_level: Level) -> FmtSubscriber<DefaultFields, Format<Json>> {
	FmtSubscriber::builder()
		.with_max_level(log_level)
		.event_format(format::json())
		.finish()
}

fn default_subscriber(log_level: Level) -> FmtSubscriber<DefaultFields, Format<Full>> {
	FmtSubscriber::builder().with_max_level(log_level).finish()
}

fn parse_log_level(log_level: &str, default: Level) -> (Level, Option<ParseLevelError>) {
	log_level
		.to_uppercase()
		.parse::<Level>()
		.map(|log_level| (log_level, None))
		.unwrap_or_else(|parse_err| (default, Some(parse_err)))
}

async fn do_main() -> Result<()> {
	let opts = CliOpts::from_args();
	let config_path = &opts.config;
	let cfg: RuntimeConfig = confy::load_path(config_path)
		.context(format!("Failed to load configuration from {config_path}"))?;

	let (log_level, parse_error) = parse_log_level(&cfg.log_level, Level::INFO);

	if cfg.log_format_json {
		tracing::subscriber::set_global_default(json_subscriber(log_level))
			.expect("global json subscriber is set")
	} else {
		tracing::subscriber::set_global_default(default_subscriber(log_level))
			.expect("global default subscriber is set")
	}

	if let Some(error) = parse_error {
		warn!("Using default log level: {}", error);
	}

	info!("Using config: {:?}", cfg);

	// Spawn Prometheus server
	let registry = Registry::default();

	let prometheus_port = cfg.prometheus_port.unwrap_or(9520);

	tokio::task::spawn(prometheus_handler::init_prometheus_with_listener(
		SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), prometheus_port),
		registry.clone(),
	));

	let db = init_db(&cfg.avail_path).context("Failed to init DB")?;

	// Have access to key value data store, now this can be safely used
	// from multiple threads of execution

	// This channel will be used for message based communication between
	// two tasks
	// task_0: HTTP request handler ( query sender )
	// task_1: IPFS client ( query receiver & hopefully successfully resolver )
	let (cell_query_tx, _) = sync_channel::<crate::types::CellContentQueryPayload>(1 << 4);
	// this spawns tokio task which runs one http server for handling RPC
	let counter = Arc::new(Mutex::new(0u64));
	tokio::task::spawn(http::run_server(
		db.clone(),
		cfg.clone(),
		cell_query_tx,
		counter.clone(),
	));

	let bootstrap_nodes = cfg
		.bootstraps
		.iter()
		.map(|(a, b)| Ok((PeerId::from_str(&a)?, b.clone())))
		.collect::<Result<Vec<(PeerId, Multiaddr)>>>()
		.context("Failed to parse bootstrap nodes")?;

	let port = if cfg.ipfs_port.1 > 0 {
		let port: u16 = thread_rng().gen_range(cfg.ipfs_port.0..=cfg.ipfs_port.1);
		info!("Using random port: {port}");
		port
	} else {
		cfg.ipfs_port.0
	};
	let seed = match cfg.ipfs_seed {
		None => thread_rng().gen(),
		Some(0) => thread_rng().gen(),
		Some(value) => value,
	};
	let ipfs = data::init_ipfs(seed, port, &cfg.ipfs_path, bootstrap_nodes)
		.await
		.context("Failed to init IPFS client")?;

	ipfs.register_metrics(&registry)
		.context("Failed to initialize IPFS Prometheus")?;

	tokio::task::spawn(data::log_ipfs_events(ipfs.clone()));

	let pp = kate_proof::testnet::public_params(1024);
	let raw_pp = pp.to_raw_var_bytes();
	let public_params_hash = hex::encode(sp_core::blake2_128(&raw_pp));
	let public_params_len = hex::encode(raw_pp).len();
	trace!("Public params ({public_params_len}): hash: {public_params_hash}");

	let rpc_url = rpc::check_http(&cfg.full_node_rpc).await?.clone();

	let version = rpc::get_chain(&rpc_url).await?;
	let runtime_version = rpc::get_runtime_version(&rpc_url).await?;
	if version != "1.1.0"
		|| runtime_version.spec_version != 5
		|| runtime_version.spec_name != "data-avail"
	{
		return Err(anyhow::anyhow!(
			"Full node is not compatible with this version of data-avail"
		));
	}

	let block_tx = if let Mode::AppClient(app_id) = Mode::from(cfg.app_id) {
		// communication channels being established for talking to
		// ipfs backed application client
		let (block_tx, block_rx) = sync_channel::<types::ClientMsg>(1 << 7);
		tokio::task::spawn(app_client::run(
			(&cfg).into(),
			ipfs.clone(),
			db.clone(),
			rpc_url.clone(),
			app_id,
			block_rx,
			pp.clone(),
		));
		Some(block_tx)
	} else {
		None
	};

	let block_header = rpc::get_chain_header(&rpc_url)
		.await
		.context(format!("Failed to get chain header from {rpc_url}"))?;
	let latest_block = block_header.number;

	// TODO: implement proper sync between bootstrap completion and starting the sync function
	thread::sleep(time::Duration::from_secs(3));

	let sync_block_depth = cfg.sync_blocks_depth.unwrap_or(0);
	if sync_block_depth > 0 {
		tokio::task::spawn(sync_client::run(
			(&cfg).into(),
			rpc_url.clone(),
			latest_block,
			sync_block_depth,
			db.clone(),
			ipfs.clone(),
			pp.clone(),
		));
	}

	// Note: if light client fails to run, process exits
	light_client::run(
		(&cfg).into(),
		db,
		ipfs,
		rpc_url,
		block_tx,
		pp,
		registry,
		counter.clone(),
	)
	.await
	.context("Failed to run light client")
}

#[tokio::main]
pub async fn main() -> Result<()> {
	do_main().await.map_err(|error| {
		error!("{error:?}");
		error
	})
}
