#![doc = include_str!("../../README.md")]

use anyhow::{anyhow, Context, Result};
use avail_core::AppId;
use avail_light::types::IdentityConfig;
use avail_light::{api, data, network::rpc, telemetry};
use avail_light::{
	consts::EXPECTED_NETWORK_VERSION,
	network::p2p,
	types::{CliOpts, Mode, RuntimeConfig, State},
};
use clap::Parser;
use kate_recovery::com::AppData;
use libp2p::{multiaddr::Protocol, Multiaddr};
use std::{
	fs,
	net::Ipv4Addr,
	path::Path,
	sync::{Arc, Mutex},
};
use tokio::sync::{
	broadcast,
	mpsc::{channel, Sender},
};
use tracing::Subscriber;
use tracing::{error, info, metadata::ParseLevelError, trace, warn, Level};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::{
	fmt::format::{self},
	FmtSubscriber,
};

#[cfg(feature = "network-analysis")]
use avail_light::network::p2p::analyzer;

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

fn json_subscriber(log_level: Level) -> impl Subscriber + Send + Sync {
	FmtSubscriber::builder()
		.with_env_filter(EnvFilter::new(format!("avail_light={log_level}")))
		.event_format(format::json())
		.finish()
}

fn default_subscriber(log_level: Level) -> impl Subscriber + Send + Sync {
	FmtSubscriber::builder()
		.with_env_filter(EnvFilter::new(format!("avail_light={log_level}")))
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

	let identity_cfg =
		IdentityConfig::load_or_init("identity.toml", opts.avail_passphrase.as_deref())?;

	let version = clap::crate_version!();
	info!("Running Avail light client version: {version}. Role: {CLIENT_ROLE}.");
	info!("Using config: {cfg:?}");
	info!("Avail address is: {}", &identity_cfg.avail_address);

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

	let db = data::init_db(&cfg.avail_path).context("Cannot initialize database")?;

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
			cfg.origin.clone(),
			identity_cfg.avail_address.clone(),
			#[cfg(feature = "crawl")]
			cfg.crawl.crawl_block_delay,
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
	info!("Listening for QUIC on port: {port}");

	p2p_client
		.start_listening(
			Multiaddr::empty()
				.with(Protocol::from(Ipv4Addr::UNSPECIFIED))
				.with(Protocol::Tcp(port + 1)),
		)
		.await
		.context("Listening on TCP not to fail.")?;
	info!("Listening for TCP on port: {port}");

	// wait here for bootstrap to finish
	info!("Bootstraping the DHT with bootstrap nodes...");
	p2p_client
		.bootstrap_on_startup(cfg.clone().bootstraps.iter().map(Into::into).collect())
		.await?;

	info!("Bootstrap done");

	#[cfg(feature = "network-analysis")]
	tokio::task::spawn(analyzer::start_traffic_analyzer(cfg.port, 10));

	let pp = Arc::new(kate_recovery::couscous::public_params());
	let raw_pp = pp.to_raw_var_bytes();
	let public_params_hash = hex::encode(sp_core::blake2_128(&raw_pp));
	let public_params_len = hex::encode(raw_pp).len();
	trace!("Public params ({public_params_len}): hash: {public_params_hash}");

	let state = Arc::new(Mutex::new(State::default()));
	let (rpc_client, rpc_events, rpc_event_loop) =
		rpc::init(db.clone(), state.clone(), &cfg.full_node_ws);

	let publish_rpc_event_receiver = rpc_events.subscribe();
	let lc_rpc_event_receiver = rpc_events.subscribe();
	let first_header_rpc_event_receiver = rpc_events.subscribe();
	#[cfg(feature = "crawl")]
	let crawler_rpc_event_receiver = rpc_events.subscribe();

	// spawn the RPC Network task for Event Loop to run in the background
	tokio::spawn(rpc_event_loop.run(EXPECTED_NETWORK_VERSION));

	info!("Waiting for first finalized header...");
	let block_header = rpc::wait_for_finalized_header(first_header_rpc_event_receiver, 60).await?;

	state.lock().unwrap().latest = block_header.number;
	let sync_range = cfg.sync_range(block_header.number);

	let ws_clients = api::v2::types::WsClients::default();

	// Spawn tokio task which runs one http server for handling RPC
	let server = api::server::Server {
		db: db.clone(),
		cfg: cfg.clone(),
		identity_cfg,
		state: state.clone(),
		version: format!("v{}", clap::crate_version!()),
		network_version: EXPECTED_NETWORK_VERSION.to_string(),
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
			sync_range.clone(),
			data_tx,
			error_sender.clone(),
		));
		(Some(block_tx), Some(data_rx))
	} else {
		(None, None)
	};

	tokio::task::spawn(api::v2::publish(
		api::v2::types::Topic::HeaderVerified,
		publish_rpc_event_receiver,
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
			crawler_rpc_event_receiver,
			p2p_client.clone(),
			cfg.crawl.crawl_block_delay,
			ot_metrics.clone(),
			cfg.crawl.crawl_block_mode,
		));
	}

	let sync_client =
		avail_light::sync_client::new(db.clone(), p2p_client.clone(), rpc_client.clone());

	if cfg.sync_start_block.is_some() {
		state.lock().unwrap().synced.replace(false);
		tokio::task::spawn(avail_light::sync_client::run(
			sync_client,
			(&cfg).into(),
			sync_range,
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
			block_header.clone(),
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
		rpc_event_receiver: lc_rpc_event_receiver,
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
