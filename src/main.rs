#![doc = include_str!("../README.md")]

use std::{
	net::{IpAddr, Ipv4Addr, SocketAddr},
	str::FromStr,
	sync::{mpsc::sync_channel, Arc, Mutex},
};

use anyhow::{Context, Result};
use async_std::stream::StreamExt;
use libp2p::{metrics::Metrics as LibP2PMetrics, Multiaddr, PeerId};
use prometheus_client::registry::Registry;
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
mod network;
mod proof;
mod rpc;
mod sync_client;
mod telemetry;
mod types;
mod utils;

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
	FmtSubscriber::builder()
		.with_max_level(log_level)
		.with_span_events(format::FmtSpan::CLOSE)
		.finish()
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
	let mut metric_registry = Registry::default();
	let libp2p_metrics = LibP2PMetrics::new(&mut metric_registry);
	let lc_metrics = telemetry::metrics::Metrics::new(&mut metric_registry);
	let prometheus_port = cfg.prometheus_port.unwrap_or(9520);

	tokio::task::spawn(telemetry::http_server(
		SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), prometheus_port),
		metric_registry,
	));

	let db = init_db(&cfg.avail_path).context("Failed to init DB")?;

	// Have access to key value data store, now this can be safely used
	// from multiple threads of execution

	// This channel will be used for message based communication between
	// two tasks
	// task_0: HTTP request handler ( query sender )
	// task_1: DHT client ( query receiver & hopefully successfully resolver )
	let (cell_query_tx, _) = sync_channel::<crate::types::CellContentQueryPayload>(1 << 4);
	// this spawns tokio task which runs one http server for handling RPC
	let counter = Arc::new(Mutex::new(0u32));
	tokio::task::spawn(http::run_server(
		db.clone(),
		cfg.clone(),
		cell_query_tx,
		counter.clone(),
	));

	let (network_client, network_events, network_event_loop) = network::init(
		cfg.libp2p_seed,
		&cfg.libp2p_psk_path,
		libp2p_metrics,
		cfg.libp2p_tcp_port_reuse,
	)
	.context("Failed to init Network Service")?;

	// Spawn the network task for it to run in the background
	tokio::spawn(network_event_loop.run());

	// Start listening on provided port
	let port = if cfg.libp2p_port.1 > 0 {
		let port: u16 = thread_rng().gen_range(cfg.libp2p_port.0..=cfg.libp2p_port.1);
		info!("Using random port: {port}");
		port
	} else {
		cfg.libp2p_port.0
	};
	network_client
		.start_listening(format!("/ip4/0.0.0.0/tcp/{}", port).parse()?)
		.await
		.context("Listening not to fail.")?;

	// Check if bootstrap nodes were provided
	let bootstrap_nodes = cfg
		.bootstraps
		.iter()
		.map(|(a, b)| Ok((PeerId::from_str(&a)?, b.clone())))
		.collect::<Result<Vec<(PeerId, Multiaddr)>>>()
		.context("Failed to parse bootstrap nodes")?;

	// If the client is the first one on the network, and no bootstrap nodes
	// were provided, then wait for the second client to establish connection and use it as bootstrap.
	// DHT requires node to be bootstrapped in order for Kademlia to be able to insert new records.
	let bootstrap_nodes = if bootstrap_nodes.is_empty() {
		info!("No bootstrap nodes, waiting for first peer to connect...");
		let node = network_events
			.stream()
			.await
			.find_map(|e| match e {
				network::Event::ConnectionEstablished { peer_id, endpoint } => {
					if endpoint.is_listener() {
						Some((peer_id, endpoint.get_remote_address().clone()))
					} else {
						None
					}
				},
			})
			// hang in there, until someone dials us
			.await
			.context("Connection is not established")?;
		vec![node]
	} else {
		bootstrap_nodes
	};

	// wait here for bootstrap to finish
	info!("Bootstraping the DHT with bootstrap nodes...");
	network_client.bootstrap(bootstrap_nodes).await?;

	let pp = kate_proof::testnet::public_params(1024);
	let raw_pp = pp.to_raw_var_bytes();
	let public_params_hash = hex::encode(sp_core::blake2_128(&raw_pp));
	let public_params_len = hex::encode(raw_pp).len();
	trace!("Public params ({public_params_len}): hash: {public_params_hash}");

	let rpc_url = rpc::check_http(&cfg.full_node_rpc).await?.clone();

	let version = rpc::get_system_version(&rpc_url).await?;
	let runtime_version = rpc::get_runtime_version(&rpc_url).await?;

	info!("Reported version: {version}, runtime version: {runtime_version:?}");
	if version != "1.4.0"
		|| runtime_version.spec_version != 7
		|| runtime_version.spec_name != "data-avail"
	{
		return Err(anyhow::anyhow!(
			"Expected node version 1.4.0 and spec 7, instead of {} and spec {}",
			version,
			runtime_version.spec_version,
		));
	}

	let block_tx = if let Mode::AppClient(app_id) = Mode::from(cfg.app_id) {
		// communication channels being established for talking to
		// libp2p backed application client
		let (block_tx, block_rx) = sync_channel::<types::ClientMsg>(1 << 7);
		tokio::task::spawn(app_client::run(
			(&cfg).into(),
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

	let sync_block_depth = cfg.sync_blocks_depth.unwrap_or(0);
	if sync_block_depth > 0 {
		tokio::task::spawn(sync_client::run(
			(&cfg).into(),
			rpc_url.clone(),
			latest_block,
			sync_block_depth,
			db.clone(),
			network_client.clone(),
			pp.clone(),
		));
	}

	// Note: if light client fails to run, process exits
	light_client::run(
		(&cfg).into(),
		db,
		network_client,
		rpc_url,
		block_tx,
		pp,
		lc_metrics,
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
