#![doc = include_str!("../README.md")]

use std::{
	net::{IpAddr, Ipv4Addr, SocketAddr},
	str::FromStr,
	sync::{Arc, Mutex},
	time::Instant,
};

use anyhow::{anyhow, Context, Result};
use async_std::stream::StreamExt;
use avail_subxt::primitives::Header;
use clap::Parser;
use consts::STATE_CF;
use libp2p::{metrics::Metrics as LibP2PMetrics, multiaddr::Protocol, Multiaddr, PeerId};
use prometheus_client::registry::Registry;
use rand::{thread_rng, Rng};
use rocksdb::{ColumnFamilyDescriptor, Options, DB};
use tokio::sync::mpsc::{channel, Sender};
use tracing::{error, info, metadata::ParseLevelError, trace, warn, Level};
use tracing_subscriber::{
	fmt::format::{self, DefaultFields, Format, Full, Json},
	FmtSubscriber,
};

use crate::{
	consts::{APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF},
	data::store_last_full_node_ws_in_db,
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
mod subscriptions;
mod sync_client;
mod telemetry;
mod types;
mod utils;

/// Light Client for Avail Blockchain
#[derive(Parser)]
#[command(version)]
struct Cli {
	/// Path to the yaml configuration file
	#[arg(short, long, value_name = "FILE", default_value_t = String::from("config.yaml"))]
	config: String,
}

fn init_db(path: &str) -> Result<Arc<DB>> {
	let mut confidence_cf_opts = Options::default();
	confidence_cf_opts.set_max_write_buffer_number(16);

	let mut block_header_cf_opts = Options::default();
	block_header_cf_opts.set_max_write_buffer_number(16);

	let mut app_data_cf_opts = Options::default();
	app_data_cf_opts.set_max_write_buffer_number(16);

	let mut state_cf_opts = Options::default();
	state_cf_opts.set_max_write_buffer_number(16);

	let cf_opts = vec![
		ColumnFamilyDescriptor::new(CONFIDENCE_FACTOR_CF, confidence_cf_opts),
		ColumnFamilyDescriptor::new(BLOCK_HEADER_CF, block_header_cf_opts),
		ColumnFamilyDescriptor::new(APP_DATA_CF, app_data_cf_opts),
		ColumnFamilyDescriptor::new(STATE_CF, state_cf_opts),
	];

	let mut db_opts = Options::default();
	db_opts.create_if_missing(true);
	db_opts.create_missing_column_families(true);

	let db = DB::open_cf_descriptors(&db_opts, path, cf_opts)?;
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

async fn run(error_sender: Sender<anyhow::Error>) -> Result<()> {
	let cli = Cli::parse();
	let config_path = &cli.config;

	let cfg: RuntimeConfig = confy::load_path(&config_path)
		.context(format!("Failed to load configuration from {config_path}"))?;

	info!("Using config: {cfg:?}");

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

	// Spawn Prometheus server
	let mut metric_registry = Registry::default();
	let libp2p_metrics = LibP2PMetrics::new(&mut metric_registry);
	let lc_metrics = telemetry::metrics::Metrics::new(&mut metric_registry);
	let prometheus_port = cfg.prometheus_port;

	let incoming = telemetry::bind(SocketAddr::new(
		IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
		prometheus_port,
	))
	.await
	.context("Cannot bind prometheus server to given address and port")?;
	tokio::task::spawn(telemetry::http_server(incoming, metric_registry));

	let db = init_db(&cfg.avail_path).context("Cannot initialize database")?;

	// Spawn tokio task which runs one http server for handling RPC
	let counter = Arc::new(Mutex::new(0u32));
	tokio::task::spawn(http::run_server(db.clone(), cfg.clone(), counter.clone()));

	let (network_client, network_event_loop) = network::init(
		(&cfg).into(),
		libp2p_metrics,
		cfg.dht_parallelization_limit,
		cfg.kad_record_ttl,
		cfg.put_batch_size,
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

	// always listen on UDP to prioritize QUIC
	network_client
		.start_listening(
			Multiaddr::empty()
				.with(Protocol::from(Ipv4Addr::UNSPECIFIED))
				.with(Protocol::Udp(port))
				.with(Protocol::QuicV1),
		)
		.await
		.context("Listening on UDP not to fail.")?;
	// if there were no provided relays, that means we're one of those
	// than we need to act as a Relay, listen on TCP also
	if cfg.relays.is_empty() {
		network_client
			.start_listening(
				Multiaddr::empty()
					.with(Protocol::from(Ipv4Addr::UNSPECIFIED))
					.with(Protocol::Tcp(port)),
			)
			.await
			.context("Listening on TCP not to fail.")?;
	}

	// Check if bootstrap nodes were provided
	let bootstrap_nodes = cfg
		.bootstraps
		.iter()
		.map(|(a, b)| Ok((PeerId::from_str(a)?, b.clone())))
		.collect::<Result<Vec<(PeerId, Multiaddr)>>>()
		.context("Failed to parse bootstrap nodes")?;

	// If the client is the first one on the network, and no bootstrap nodes
	// were provided, then wait for the second client to establish connection and use it as bootstrap.
	// DHT requires node to be bootstrapped in order for Kademlia to be able to insert new records.
	let bootstrap_nodes = if bootstrap_nodes.is_empty() {
		info!("No bootstrap nodes, waiting for first peer to connect...");
		let node = network_client
			.events_stream()
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

	let pp = kate_recovery::testnet::public_params(1024);
	let raw_pp = pp.to_raw_var_bytes();
	let public_params_hash = hex::encode(sp_core::blake2_128(&raw_pp));
	let public_params_len = hex::encode(raw_pp).len();
	trace!("Public params ({public_params_len}): hash: {public_params_hash}");

	let last_full_node_ws = data::get_last_full_node_ws_from_db(db.clone())?;

	let version = rpc::Version {
		version: "1.6.1".to_string(),
		spec_version: 10,
		spec_name: "data-avail".to_string(),
	};
	let (rpc_client, last_full_node_ws) =
		rpc::connect_to_the_full_node(&cfg.full_node_ws, last_full_node_ws, version).await?;

	store_last_full_node_ws_in_db(db.clone(), last_full_node_ws)?;

	let block_tx = if let Mode::AppClient(app_id) = Mode::from(cfg.app_id) {
		// communication channels being established for talking to
		// libp2p backed application client
		let (block_tx, block_rx) = channel::<types::BlockVerified>(1 << 7);
		tokio::task::spawn(app_client::run(
			(&cfg).into(),
			db.clone(),
			network_client.clone(),
			rpc_client.clone(),
			app_id,
			block_rx,
			pp.clone(),
		));
		Some(block_tx)
	} else {
		None
	};

	let block_header = rpc::get_chain_header(&rpc_client)
		.await
		.context(format!("Failed to get chain header from {rpc_client:?}"))?;
	let latest_block = block_header.number;

	let sync_block_depth = cfg.sync_blocks_depth.unwrap_or(0);
	if sync_block_depth > 0 {
		tokio::task::spawn(sync_client::run(
			(&cfg).into(),
			rpc_client.clone(),
			latest_block,
			sync_block_depth,
			db.clone(),
			network_client.clone(),
			pp.clone(),
			block_tx.clone(),
		));
	}

	let (message_tx, message_rx) = channel::<(Header, Instant)>(128);

	tokio::task::spawn(subscriptions::finalized_headers(
		rpc_client.clone(),
		message_tx,
		error_sender.clone(),
	));

	tokio::task::spawn(light_client::run(
		(&cfg).into(),
		db,
		network_client,
		rpc_client,
		block_tx,
		pp,
		lc_metrics,
		counter.clone(),
		message_rx,
		error_sender,
	));
	Ok(())
}

#[tokio::main]
pub async fn main() -> Result<()> {
	let (error_sender, mut error_receiver) = channel::<anyhow::Error>(1);

	if let Err(error) = run(error_sender).await {
		error!("{error}");
		return Err(error);
	};

	let error = match error_receiver.recv().await {
		Some(error) => error,
		None => anyhow!("Failed to receive error message"),
	};

	// We are not logging error here since expectation is
	// to log terminating condition before sending message to this channel
	Err(error)
}
