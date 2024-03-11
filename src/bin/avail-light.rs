#![doc = include_str!("../../README.md")]

use avail_core::AppId;
use avail_light::{
	api,
	consts::EXPECTED_SYSTEM_VERSION,
	data::rocks_db::RocksDB,
	maintenance::StaticConfigParams,
	network::{self, p2p, rpc},
	shutdown::Controller,
	sync_client::SyncClient,
	sync_finality::SyncFinality,
	telemetry::{self, otlp::MetricAttributes},
	types::{CliOpts, IdentityConfig, LibP2PConfig, RuntimeConfig, State},
};
use clap::Parser;
use color_eyre::{
	eyre::{eyre, WrapErr},
	Result,
};
use kate_recovery::com::AppData;
use libp2p::{multiaddr::Protocol, Multiaddr};
use std::{
	fs,
	net::Ipv4Addr,
	path::Path,
	sync::{Arc, Mutex},
};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{error, info, metadata::ParseLevelError, trace, warn, Level, Subscriber};
use tracing_subscriber::{fmt::format, EnvFilter, FmtSubscriber};

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

async fn run(shutdown: Controller<String>) -> Result<()> {
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
		IdentityConfig::load_or_init(&opts.identity, opts.avail_passphrase.as_deref())?;
	info!("Identity loaded from {}", &opts.identity);

	let client_role = if cfg.is_fat_client() {
		info!("Fat client mode");
		"fatnode"
	} else {
		CLIENT_ROLE
	};

	let version = clap::crate_version!();
	info!("Running Avail light client version: {version}. Role: {client_role}.");
	info!("Using config: {cfg:?}");
	info!("Avail address is: {}", &identity_cfg.avail_address);

	if let Some(error) = parse_error {
		warn!("Using default log level: {}", error);
	}

	if opts.clean && Path::new(&cfg.avail_path).exists() {
		info!("Cleaning up local state directory");
		fs::remove_dir_all(&cfg.avail_path).wrap_err("Failed to remove local state directory")?;
	}

	if cfg.bootstraps.is_empty() {
		Err(eyre!("Bootstrap node list must not be empty. Either use a '--network' flag or add a list of bootstrap nodes in the configuration file"))?
	}

	let db =
		RocksDB::open(&cfg.avail_path).wrap_err("Avail Light could not initialize database")?;

	let cfg_libp2p: LibP2PConfig = (&cfg).into();
	let (id_keys, peer_id) = p2p::keypair(&cfg_libp2p)?;

	let metric_attributes = MetricAttributes {
		role: client_role.into(),
		peer_id,
		ip: RwLock::new("".to_string()),
		multiaddress: RwLock::new("".to_string()), // Default value is empty until first processed block triggers an update,
		origin: cfg.origin.clone(),
		avail_address: identity_cfg.avail_address.clone(),
		operating_mode: cfg.operation_mode.to_string(),
		partition_size: cfg
			.block_matrix_partition
			.map(|_| {
				format!(
					"{}/{}",
					cfg.block_matrix_partition
						.expect("partition doesn't exist")
						.number,
					cfg.block_matrix_partition
						.expect("partition doesn't exist")
						.fraction
				)
			})
			.unwrap_or("n/a".to_string()),
	};

	let ot_metrics = Arc::new(
		telemetry::otlp::initialize(cfg.ot_collector_endpoint.clone(), metric_attributes)
			.wrap_err("Unable to initialize OpenTelemetry service")?,
	);

	// Create sender channel for P2P event loop commands
	let (p2p_event_loop_sender, p2p_event_loop_receiver) = mpsc::unbounded_channel();

	let p2p_event_loop = p2p::EventLoop::new(
		cfg_libp2p,
		&id_keys,
		cfg.is_fat_client(),
		cfg.ws_transport_enable,
		shutdown.clone(),
	);

	tokio::spawn(
		shutdown.with_cancel(
			p2p_event_loop
				.await
				.run(ot_metrics.clone(), p2p_event_loop_receiver),
		),
	);

	let p2p_client = p2p::Client::new(
		p2p_event_loop_sender,
		cfg.dht_parallelization_limit,
		cfg.kad_record_ttl,
	);

	// Start listening on provided port
	p2p_client
		.start_listening(construct_multiaddress(cfg.ws_transport_enable, cfg.port))
		.await
		.wrap_err("Listening on TCP not to fail.")?;
	info!("TCP listener started on port {}", cfg.port);

	let p2p_clone = p2p_client.to_owned();
	let cfg_clone = cfg.to_owned();
	tokio::spawn(shutdown.with_cancel(async move {
		info!("Bootstraping the DHT with bootstrap nodes...");
		let bs_result = p2p_clone
			.bootstrap_on_startup(cfg_clone.bootstraps.iter().map(Into::into).collect())
			.await;
		match bs_result {
			Ok(_) => {
				info!("Bootstrap done.");
			},
			Err(e) => {
				warn!("Bootstrap process: {e:?}.");
			},
		}
	}));

	#[cfg(feature = "network-analysis")]
	tokio::task::spawn(shutdown.with_cancel(analyzer::start_traffic_analyzer(cfg.port, 10)));

	let pp = Arc::new(kate_recovery::couscous::public_params());
	let raw_pp = pp.to_raw_var_bytes();
	let public_params_hash = hex::encode(sp_core::blake2_128(&raw_pp));
	let public_params_len = hex::encode(raw_pp).len();
	trace!("Public params ({public_params_len}): hash: {public_params_hash}");

	let state = Arc::new(Mutex::new(State::default()));
	let (rpc_client, rpc_events, rpc_subscriptions) = rpc::init(
		db.clone(),
		state.clone(),
		&cfg.full_node_ws,
		&cfg.genesis_hash,
		cfg.retry_config.clone(),
	)
	.await?;

	// Subscribing to RPC events before first event is published
	let publish_rpc_event_receiver = rpc_events.subscribe();
	let first_header_rpc_event_receiver = rpc_events.subscribe();
	let client_rpc_event_receiver = rpc_events.subscribe();
	#[cfg(feature = "crawl")]
	let crawler_rpc_event_receiver = rpc_events.subscribe();

	// spawn the RPC Network task for Event Loop to run in the background
	// and shut it down, without delays
	let rpc_subscriptions_handle = tokio::spawn(shutdown.with_cancel(shutdown.with_trigger(
		"Subscription loop failure triggered shutdown".to_string(),
		async {
			let result = rpc_subscriptions.run().await;
			if let Err(ref err) = result {
				error!(%err, "Subscription loop ended with error");
			};
			result
		},
	)));

	info!("Waiting for first finalized header...");
	let block_header = match shutdown
		.with_cancel(rpc::wait_for_finalized_header(
			first_header_rpc_event_receiver,
			360,
		))
		.await
	{
		Ok(Err(report)) => {
			if !rpc_subscriptions_handle.is_finished() {
				return Err(report);
			}
			let Ok(Ok(Err(subscriptions_error))) = rpc_subscriptions_handle.await else {
				return Err(report);
			};
			return Err(eyre!(subscriptions_error));
		},
		Ok(Ok(num)) => num,
		Err(shutdown_reason) => {
			if !rpc_subscriptions_handle.is_finished() {
				return Err(eyre!(shutdown_reason));
			}
			let Ok(Ok(Err(event_loop_error))) = rpc_subscriptions_handle.await else {
				return Err(eyre!(shutdown_reason));
			};
			return Err(eyre!(event_loop_error));
		},
	};

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
		network_version: EXPECTED_SYSTEM_VERSION[0].to_string(),
		node_client: rpc_client.clone(),
		ws_clients: ws_clients.clone(),
		shutdown: shutdown.clone(),
	};
	tokio::task::spawn(shutdown.with_cancel(server.bind()));

	let (block_tx, block_rx) = broadcast::channel::<avail_light::types::BlockVerified>(1 << 7);

	let data_rx = cfg.app_id.map(AppId).map(|app_id| {
		let (data_tx, data_rx) = broadcast::channel::<(u32, AppData)>(1 << 7);
		tokio::task::spawn(shutdown.with_cancel(avail_light::app_client::run(
			(&cfg).into(),
			db.clone(),
			p2p_client.clone(),
			rpc_client.clone(),
			app_id,
			block_tx.subscribe(),
			pp.clone(),
			state.clone(),
			sync_range.clone(),
			data_tx,
			shutdown.clone(),
		)));
		data_rx
	});

	tokio::task::spawn(shutdown.with_cancel(api::v2::publish(
		api::v2::types::Topic::HeaderVerified,
		publish_rpc_event_receiver,
		ws_clients.clone(),
	)));

	tokio::task::spawn(shutdown.with_cancel(api::v2::publish(
		api::v2::types::Topic::ConfidenceAchieved,
		block_tx.subscribe(),
		ws_clients.clone(),
	)));

	if let Some(data_rx) = data_rx {
		tokio::task::spawn(shutdown.with_cancel(api::v2::publish(
			api::v2::types::Topic::DataVerified,
			data_rx,
			ws_clients,
		)));
	}

	#[cfg(feature = "crawl")]
	if cfg.crawl.crawl_block {
		let partition = cfg.crawl.crawl_block_matrix_partition;
		tokio::task::spawn(shutdown.with_cancel(avail_light::crawl_client::run(
			crawler_rpc_event_receiver,
			p2p_client.clone(),
			cfg.crawl.crawl_block_delay,
			ot_metrics.clone(),
			cfg.crawl.crawl_block_mode,
			partition.unwrap_or(avail_light::crawl_client::ENTIRE_BLOCK),
		)));
	}

	let sync_client = SyncClient::new(db.clone(), rpc_client.clone());

	let sync_network_client = network::new(
		p2p_client.clone(),
		rpc_client.clone(),
		pp.clone(),
		cfg.disable_rpc,
	);

	if cfg.sync_start_block.is_some() {
		state.lock().unwrap().synced.replace(false);
		tokio::task::spawn(shutdown.with_cancel(avail_light::sync_client::run(
			sync_client,
			sync_network_client,
			(&cfg).into(),
			sync_range,
			block_tx.clone(),
			state.clone(),
		)));
	}

	if cfg.sync_finality_enable {
		let sync_finality = SyncFinality::new(db.clone(), rpc_client.clone());
		tokio::task::spawn(shutdown.with_cancel(avail_light::sync_finality::run(
			sync_finality,
			shutdown.clone(),
			state.clone(),
			block_header.clone(),
		)));
	} else {
		let mut s = state
			.lock()
			.map_err(|e| eyre!("State mutex is poisoned: {e:#}"))?;
		warn!("Finality sync is disabled! Implicitly, blocks before LC startup will be considered verified as final");
		s.finality_synced = true;
	}

	let static_config_params = StaticConfigParams {
		block_confidence_treshold: cfg.confidence,
		replication_factor: cfg.replication_factor,
		query_timeout: cfg.query_timeout,
		pruning_interval: cfg.store_pruning_interval,
	};

	tokio::task::spawn(shutdown.with_cancel(avail_light::maintenance::run(
		p2p_client.clone(),
		ot_metrics.clone(),
		block_rx,
		static_config_params,
		shutdown.clone(),
	)));

	let channels = avail_light::types::ClientChannels {
		block_sender: block_tx,
		rpc_event_receiver: client_rpc_event_receiver,
	};

	if let Some(partition) = cfg.block_matrix_partition {
		let fat_client = avail_light::fat_client::new(p2p_client.clone(), rpc_client.clone());

		tokio::task::spawn(shutdown.with_cancel(avail_light::fat_client::run(
			fat_client,
			db.clone(),
			(&cfg).into(),
			ot_metrics.clone(),
			channels,
			partition,
			shutdown.clone(),
		)));
	} else {
		let light_network_client = network::new(p2p_client, rpc_client, pp, cfg.disable_rpc);

		tokio::task::spawn(shutdown.with_cancel(avail_light::light_client::run(
			db.clone(),
			light_network_client,
			(&cfg).into(),
			ot_metrics,
			state.clone(),
			channels,
			shutdown.clone(),
		)));
	}

	Ok(())
}

fn construct_multiaddress(is_websocket: bool, port: u16) -> Multiaddr {
	let tcp_multiaddress = Multiaddr::empty()
		.with(Protocol::from(Ipv4Addr::UNSPECIFIED))
		.with(Protocol::Tcp(port));

	if is_websocket {
		return tcp_multiaddress.with(Protocol::Ws(std::borrow::Cow::Borrowed("avail-light")));
	}

	tcp_multiaddress
}

fn install_panic_hooks(shutdown: Controller<String>) -> Result<()> {
	// initialize color-eyre hooks
	let (panic_hook, eyre_hook) = color_eyre::config::HookBuilder::default()
		.display_location_section(true)
		.display_env_section(true)
		.into_hooks();

	// install hook as global handler
	eyre_hook.install()?;

	std::panic::set_hook(Box::new(move |panic_info| {
		// trigger shutdown to stop other tasks if panic occurs
		let _ = shutdown.trigger_shutdown("Panic occurred, shuting down".to_string());

		let msg = format!("{}", panic_hook.panic_report(panic_info));
		error!("Error: {}", strip_ansi_escapes::strip_str(msg));

		#[cfg(debug_assertions)]
		{
			// better-panic stacktrace that is only enabled when debugging
			better_panic::Settings::auto()
				.most_recent_first(false)
				.lineno_suffix(true)
				.verbosity(better_panic::Verbosity::Medium)
				.create_panic_handler()(panic_info);
		}
	}));
	Ok(())
}

/// This utility function returns a [`Future`] that completes upon
/// receiving each of the default termination signals.
///
/// On Unix-based systems, these signals are Ctrl-C (SIGINT) or SIGTERM,
/// and on Windows, they are Ctrl-C, Ctrl-Close, Ctrl-Shutdown.
async fn user_signal() {
	let ctrl_c = tokio::signal::ctrl_c();
	#[cfg(all(unix, not(windows)))]
	{
		let sig = async {
			let mut os_sig =
				tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
			os_sig.recv().await;
			std::io::Result::Ok(())
		};

		tokio::select! {
			_ = ctrl_c => {},
			_ = sig => {}
		}
	}

	#[cfg(all(not(unix), windows))]
	{
		let ctrl_close = async {
			let mut sig = tokio::signal::windows::ctrl_close()?;
			sig.recv().await;
			std::io::Result::Ok(())
		};
		let ctrl_shutdown = async {
			let mut sig = tokio::signal::windows::ctrl_shutdown()?;
			sig.recv().await;
			std::io::Result::Ok(())
		};
		tokio::select! {
			_ = ctrl_c => {},
			_ = ctrl_close => {},
			_ = ctrl_shutdown => {},
		}
	}
}

#[tokio::main]
pub async fn main() -> Result<()> {
	let shutdown = Controller::new();

	// install custom panic hooks
	install_panic_hooks(shutdown.clone())?;

	// spawn a task to watch for ctrl-c signals from user to trigger the shutdown
	tokio::spawn(shutdown.with_trigger("user signaled shutdown".to_string(), user_signal()));

	if let Err(error) = run(shutdown.clone()).await {
		error!("{error:#}");
		return Err(error.wrap_err("Starting Light Client failed"));
	};

	let reason = shutdown.completed_shutdown().await;

	// we are not logging error here since expectation is
	// to log terminating condition before sending message to this channel
	Err(eyre!(reason).wrap_err("Running Light Client encountered an error"))
}
