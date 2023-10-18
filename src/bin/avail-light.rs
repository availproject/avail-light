#![doc = include_str!("../../README.md")]

use anyhow::{anyhow, Context, Result};
use avail_core::AppId;
use avail_light::{
	api,
	consts::STATE_CF,
	network::rpc,
	telemetry::{self},
};
use avail_light::{
	consts::{APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF, EXPECTED_NETWORK_VERSION},
	network::p2p::{self},
	types::{CliOpts, Mode, RuntimeConfig, State},
};
use clap::Parser;
use kate_recovery::com::AppData;
use libp2p::{multiaddr::Protocol, Multiaddr};
use rocksdb::{ColumnFamilyDescriptor, Options, DB};
use std::{fs, path::Path};
use std::{
	net::Ipv4Addr,
	sync::{Arc, Mutex},
};
use tokio::sync::{
	broadcast,
	mpsc::{channel, Sender},
};
use tracing::{error, info, metadata::ParseLevelError, trace, warn, Level};
use tracing_subscriber::{
	fmt::format::{self, DefaultFields, Format, Full, Json},
	FmtSubscriber,
};

#[cfg(feature = "network-analysis")]
use avail_light::network::analyzer;

#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

const CLIENT_ROLE: &str = if cfg!(feature = "crawl") {
	"crawler"
} else {
	"lightnode"
};

/// Light Client for Avail Blockchain

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

	let mut cfg: RuntimeConfig = RuntimeConfig::default();
	cfg.load_runtime_config(&opts)?;

	let (log_level, parse_error) = parse_log_level(&cfg.log_level, Level::INFO);

	if cfg.log_format_json {
		tracing::subscriber::set_global_default(json_subscriber(log_level))
			.expect("global json subscriber is set")
	} else {
		tracing::subscriber::set_global_default(default_subscriber(log_level))
			.expect("global default subscriber is set")
	}
	let version = clap::crate_version!();
	info!("Running Avail light client version: {version}");
	info!("Using config: {cfg:?}");

	if let Some(error) = parse_error {
		warn!("Using default log level: {}", error);
	}

	if opts.clean && Path::new(&cfg.avail_path).exists() {
		info!("Cleaning up local state directory");
		fs::remove_dir_all(&cfg.avail_path).context("Failed to remove local state directory")?;
	}

	if cfg.bootstraps.is_empty() {
		Err(anyhow!("Bootstrap node list must not be empty. Either use a '--network' flag or add a list of bootstrap nodes in the configuration file"))?
	}

	let db = init_db(&cfg.avail_path).context(
		"Cannot initialize database. Try running with '--clean' flag for a clean deployment.",
	)?;

	// If in fat client mode, enable deleting local Kademlia records
	// This is a fat client memory optimization
	let kad_remove_local_record = cfg.block_matrix_partition.is_some();
	if kad_remove_local_record {
		info!("Fat client mode");
	}

	let (id_keys, peer_id) = p2p::keypair((&cfg).into())?;

	let ot_metrics = Arc::new(
		telemetry::otlp::initialize(
			cfg.ot_collector_endpoint.clone(),
			peer_id,
			CLIENT_ROLE.into(),
		)
		.context("Unable to initialize OpenTelemetry service")?,
	);

	// raise new P2P Network Client and Event Loop
	let (p2p_client, p2p_event_loop) = p2p::init(
		(&cfg).into(),
		cfg.dht_parallelization_limit,
		cfg.kad_record_ttl,
		cfg.put_batch_size,
		kad_remove_local_record,
		id_keys,
	)
	.context("Failed to init Network Service")?;

	// spawn the P2P Network task for Event Loop run in the background
	tokio::spawn(p2p_event_loop.run());

	// Start listening on provided port
	let port = cfg.port;
	info!("Listening on port: {port}");

	// always listen on UDP to prioritize QUIC
	p2p_client
		.start_listening(
			Multiaddr::empty()
				.with(Protocol::from(Ipv4Addr::UNSPECIFIED))
				.with(Protocol::Udp(port))
				.with(Protocol::QuicV1),
		)
		.await
		.context("Listening on UDP not to fail.")?;

	// wait here for bootstrap to finish
	info!("Bootstraping the DHT with bootstrap nodes...");
	p2p_client.bootstrap(cfg.clone().bootstraps).await?;

	#[cfg(feature = "network-analysis")]
	tokio::task::spawn(analyzer::start_traffic_analyzer(cfg.port, 10));

	let pp = Arc::new(kate_recovery::testnet::public_params(1024));
	let raw_pp = pp.to_raw_var_bytes();
	let public_params_hash = hex::encode(sp_core::blake2_128(&raw_pp));
	let public_params_len = hex::encode(raw_pp).len();
	trace!("Public params ({public_params_len}): hash: {public_params_hash}");

	let state = Arc::new(Mutex::new(State::default()));
	let (rpc_client, rpc_events, rpc_event_loop) =
		rpc::init(db.clone(), state.clone(), &cfg.full_nodes_ws);
	// spawn the RPC Network task for Event Loop to run in the background
	tokio::spawn(rpc_event_loop.run(EXPECTED_NETWORK_VERSION));

	let node = rpc_client.get_connected_node().await?;
	info!("Genesis hash: {:?}", node.genesis_hash);
	if let Some(stored_genesis_hash) = avail_light::data::get_genesis_hash(db.clone())? {
		if !node.genesis_hash.eq(&stored_genesis_hash) {
			Err(anyhow!(
				"Genesis hash doesn't match the stored one! Clear the db or change nodes."
			))?
		}
	} else {
		info!("No genesis hash is found in the db, storing the new hash now.");
		avail_light::data::store_genesis_hash(db.clone(), node.genesis_hash)?;
	}

	let block_header = rpc_client
		.get_chain_head_header()
		.await
		.context("Failed to get chain header")?;

	state.lock().unwrap().latest = block_header.number;
	let sync_end_block = block_header.number.saturating_sub(1);

	let ws_clients = api::v2::types::WsClients::default();

	// Spawn tokio task which runs one http server for handling RPC
	let server = api::server::Server {
		db: db.clone(),
		cfg: cfg.clone(),
		state: state.clone(),
		version: format!("v{}", clap::crate_version!()),
		network_version: EXPECTED_NETWORK_VERSION.to_string(),
		node,
		node_client: rpc_client.clone(),
		ws_clients: ws_clients.clone(),
	};

	tokio::task::spawn(server.run());

	let (block_tx, data_rx) = if let Mode::AppClient(app_id) = Mode::from(cfg.app_id) {
		// communication channels being established for talking to
		// libp2p backed application client
		let (block_tx, block_rx) = broadcast::channel::<avail_light::types::BlockVerified>(1 << 7);
		let (data_tx, data_rx) = broadcast::channel::<(u32, AppData)>(1 << 7);
		tokio::task::spawn(avail_light::app_client::run(
			(&cfg).into(),
			db.clone(),
			p2p_client.clone(),
			rpc_client.clone(),
			AppId(app_id),
			block_rx,
			pp.clone(),
			state.clone(),
			sync_end_block,
			data_tx,
			error_sender.clone(),
		));
		(Some(block_tx), Some(data_rx))
	} else {
		(None, None)
	};

	#[cfg(feature = "api-v2")]
	{
		tokio::task::spawn(api::v2::publish(
			api::v2::types::Topic::HeaderVerified,
			rpc_events.subscribe(),
			ws_clients.clone(),
		));

	if let Some(sender) = block_tx.as_ref() {
		tokio::task::spawn(api::v2::publish(
			api::v2::types::Topic::ConfidenceAchieved,
			sender.subscribe(),
			ws_clients.clone(),
		));
	}

	if let Some(data_rx) = data_rx {
		tokio::task::spawn(api::v2::publish(
			api::v2::types::Topic::DataVerified,
			data_rx,
			ws_clients,
		));
	}

	#[cfg(feature = "crawl")]
	if cfg.crawl.crawl_block {
		tokio::task::spawn(avail_light::crawl_client::run(
			message_tx.subscribe(),
			network_client.clone(),
			cfg.crawl.crawl_block_delay,
			ot_metrics.clone(),
			cfg.crawl.crawl_block_mode,
		));
	}

	let sync_client =
		avail_light::sync_client::new(db.clone(), p2p_client.clone(), rpc_client.clone());

	if let Some(sync_start_block) = cfg.sync_start_block {
		state.lock().unwrap().synced.replace(false);
		tokio::task::spawn(avail_light::sync_client::run(
			sync_client,
			(&cfg).into(),
			sync_start_block,
			sync_end_block,
			pp.clone(),
			block_tx.clone(),
			state.clone(),
		));
	}

	if cfg.sync_finality_enable {
		let sync_finality = avail_light::sync_finality::new(db.clone(), rpc_client.clone());
		tokio::task::spawn(avail_light::sync_finality::run(
			sync_finality,
			error_sender.clone(),
			state.clone(),
		));
	} else {
		let mut s = state
			.lock()
			.map_err(|e| anyhow!("State mutex is poisoned: {e:#}"))?;
		warn!("Finality sync is disabled! Implicitly, blocks before LC startup will be considered verified as final");
		s.finality_synced = true;
	}

	let light_client =
		avail_light::light_client::new(db.clone(), p2p_client.clone(), rpc_client.clone());

	let lc_channels = avail_light::light_client::Channels {
		block_sender: block_tx,
		rpc_event_receiver: rpc_events.subscribe(),
		error_sender: error_sender.clone(),
	};

	tokio::task::spawn(avail_light::light_client::run(
		light_client,
		(&cfg).into(),
		pp,
		ot_metrics,
		state.clone(),
		lc_channels,
	));

	Ok(())
}

#[tokio::main]
pub async fn main() -> Result<()> {
	let (error_sender, mut error_receiver) = channel::<anyhow::Error>(1);

	if let Err(error) = run(error_sender).await {
		error!("{error:#}");
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
