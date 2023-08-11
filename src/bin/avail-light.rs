#![doc = include_str!("../../README.md")]

use std::{
	net::{IpAddr, Ipv4Addr, SocketAddr},
	str::FromStr,
	sync::{Arc, Mutex},
	time::Instant,
};

use anyhow::{anyhow, Context, Result};
use async_std::stream::StreamExt;
use avail_core::AppId;
use avail_light::consts::STATE_CF;
use avail_subxt::primitives::Header;
use clap::Parser;
use libp2p::{metrics::Metrics as LibP2PMetrics, multiaddr::Protocol, Multiaddr, PeerId};
use prometheus_client::registry::Registry;
use rocksdb::{ColumnFamilyDescriptor, Options, DB};
use tokio::sync::mpsc::{channel, Sender};
use tracing::{error, info, metadata::ParseLevelError, trace, warn, Level};
use tracing_subscriber::{
	fmt::format::{self, DefaultFields, Format, Full, Json},
	FmtSubscriber,
};

use avail_light::{
	consts::{APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF, EXPECTED_NETWORK_VERSION},
	data::store_last_full_node_ws_in_db,
	types::{Mode, RuntimeConfig, State},
};

#[cfg(feature = "network-analysis")]
use crate::network::network_analyzer;

#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

/// Light Client for Avail Blockchain
#[derive(Parser)]
#[command(version)]
struct CliOpts {
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
	let opts = CliOpts::parse();
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

	info!("Using config: {cfg:?}");

	if let Some(error) = parse_error {
		warn!("Using default log level: {}", error);
	}

	// Spawn Prometheus server
	let mut metric_registry = Registry::default();
	let libp2p_metrics = LibP2PMetrics::new(&mut metric_registry);
	let lc_metrics = avail_light::telemetry::metrics::Metrics::new(&mut metric_registry);
	let prometheus_port = cfg.prometheus_port;

	let incoming = avail_light::telemetry::bind(SocketAddr::new(
		IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
		prometheus_port,
	))
	.await
	.context("Cannot bind prometheus server to given address and port")?;
	tokio::task::spawn(avail_light::telemetry::http_server(
		incoming,
		metric_registry,
	));

	let db = init_db(&cfg.avail_path).context("Cannot initialize database")?;

	// Spawn tokio task which runs one http server for handling RPC
	let state = Arc::new(Mutex::new(State::default()));

	let server = avail_light::api::server::Server {
		db: db.clone(),
		cfg: cfg.clone(),
		state: state.clone(),
		version: format!("v{}", clap::crate_version!()),
		network_version: EXPECTED_NETWORK_VERSION.to_string(),
	};

	tokio::task::spawn(server.run());

	// If in fat client mode, enable deleting local Kademlia records
	// This is a fat client memory optimization
	let kad_remove_local_record = cfg.block_matrix_partition.is_some();
	if kad_remove_local_record {
		info!("Fat client mode");
	}

	let (network_client, network_event_loop) = avail_light::network::init(
		(&cfg).into(),
		libp2p_metrics,
		lc_metrics.clone(),
		cfg.dht_parallelization_limit,
		cfg.kad_record_ttl,
		cfg.put_batch_size,
		kad_remove_local_record,
	)
	.context("Failed to init Network Service")?;

	// Spawn the network task for it to run in the background
	tokio::spawn(network_event_loop.run());

	// Start listening on provided port
	let port = cfg.port;
	info!("Listening on port: {port}");

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

	// Check if bootstrap nodes were provided
	let bootstrap_nodes = cfg
		.bootstraps
		.iter()
		.map(|(a, b)| Ok((PeerId::from_str(a)?, b.clone())))
		.collect::<Result<Vec<(PeerId, Multiaddr)>>>()
		.context("Failed to parse bootstrap nodes")?;

	// If the client is the first one on the network, and no bootstrap nodes, then wait for the
	// second client to establish connection and use it as bootstrap.
	// DHT requires node to be bootstrapped in order for Kademlia to be able to insert new records.
	let bootstrap_nodes = if bootstrap_nodes.is_empty() {
		info!("No bootstrap nodes, waiting for first peer to connect...");
		let node = network_client
			.events_stream()
			.await
			.find_map(|e| match e {
				avail_light::network::Event::ConnectionEstablished { peer_id, endpoint } => {
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

	#[cfg(feature = "network-analysis")]
	tokio::task::spawn(network_analyzer::start_traffic_analyzer(cfg.port, 10));

	let pp = Arc::new(kate_recovery::testnet::public_params(1024));
	let raw_pp = pp.to_raw_var_bytes();
	let public_params_hash = hex::encode(sp_core::blake2_128(&raw_pp));
	let public_params_len = hex::encode(raw_pp).len();
	trace!("Public params ({public_params_len}): hash: {public_params_hash}");

	let last_full_node_ws = avail_light::data::get_last_full_node_ws_from_db(db.clone())?;

	let (rpc_client, last_full_node_ws) = avail_light::rpc::connect_to_the_full_node(
		&cfg.full_node_ws,
		last_full_node_ws,
		EXPECTED_NETWORK_VERSION,
	)
	.await?;

	store_last_full_node_ws_in_db(db.clone(), last_full_node_ws)?;

	let genesis_hash = rpc_client.genesis_hash();
	info!("Genesis hash: {genesis_hash:?}");
	if let Some(stored_genesis_hash) = avail_light::data::get_genesis_hash(db.clone())? {
		if !genesis_hash.eq(&stored_genesis_hash) {
			Err(anyhow!(
				"Genesis hash doesn't match the stored one! Clear the db or change nodes."
			))?
		}
	} else {
		info!("No genesis hash is found in the db, storing the new hash now.");
		avail_light::data::store_genesis_hash(db.clone(), genesis_hash)?;
	}

	let block_tx = if let Mode::AppClient(app_id) = Mode::from(cfg.app_id) {
		// communication channels being established for talking to
		// libp2p backed application client
		let (block_tx, block_rx) = channel::<avail_light::types::BlockVerified>(1 << 7);
		tokio::task::spawn(avail_light::app_client::run(
			(&cfg).into(),
			db.clone(),
			network_client.clone(),
			rpc_client.clone(),
			AppId(app_id),
			block_rx,
			pp.clone(),
		));
		Some(block_tx)
	} else {
		None
	};

	let block_header = avail_light::rpc::get_chain_head_header(&rpc_client)
		.await
		.context(format!("Failed to get chain header from {rpc_client:?}"))?;
	let latest_block = block_header.number;

	let sync_client =
		avail_light::sync_client::new(db.clone(), network_client.clone(), rpc_client.clone());

	if let Some(sync_start_block) = cfg.sync_start_block {
		tokio::task::spawn(avail_light::sync_client::run(
			sync_client,
			(&cfg).into(),
			sync_start_block,
			latest_block,
			pp.clone(),
			block_tx.clone(),
		));
	}

	let (message_tx, message_rx) = channel::<(Header, Instant)>(128);

	tokio::task::spawn(avail_light::subscriptions::finalized_headers(
		rpc_client.clone(),
		message_tx,
		error_sender.clone(),
	));

	let light_client = avail_light::light_client::new(db, network_client, rpc_client);

	let lc_channels = avail_light::light_client::Channels {
		block_sender: block_tx,
		header_receiver: message_rx,
		error_sender,
	};

	tokio::task::spawn(avail_light::light_client::run(
		light_client,
		(&cfg).into(),
		pp,
		lc_metrics,
		state,
		lc_channels,
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
