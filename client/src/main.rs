#![doc = include_str!("../README.md")]

use crate::cli::{CliOpts, Network};
use avail_light_core::{
	data::{ClientIdKey, Database, LatestHeaderKey, P2PKeypairKey, RocksDB},
	network::{
		p2p::{self, configuration::LibP2PConfig},
		rpc,
	},
	shutdown::Controller,
	telemetry::{self, otlp::MetricAttributes, MetricCounter, Metrics},
	types::{
		load_or_init_suri, IdentityConfig, KademliaMode, MaintenanceConfig, MultiaddrConfig,
		RuntimeConfig, SecretKey, Uuid,
	},
	utils::{install_panic_hooks, spawn_in_span},
};
use clap::Parser;
use color_eyre::{
	eyre::{eyre, WrapErr},
	Result,
};
use kate_recovery::matrix::Partition;
use libp2p::{
	identity::{self, ed25519},
	Multiaddr, PeerId,
};
use std::{fs, path::Path, str::FromStr, sync::Arc};
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, metadata::ParseLevelError, span, warn, Level, Subscriber};
use tracing_subscriber::{fmt::format, EnvFilter, FmtSubscriber};

#[cfg(not(feature = "crawl"))]
use avail_core::AppId;

#[cfg(not(feature = "crawl"))]
use avail_light_core::{
	api,
	consts::EXPECTED_SYSTEM_VERSION,
	data::{IsFinalitySyncedKey, IsSyncedKey},
	network,
	sync_client::SyncClient,
	sync_finality::SyncFinality,
};

#[cfg(not(feature = "crawl"))]
use kate_recovery::com::AppData;

#[cfg(not(feature = "crawl"))]
use tracing::trace;

#[cfg(feature = "network-analysis")]
#[cfg(not(feature = "crawl"))]
use avail_light_core::network::p2p::analyzer;

#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

/// Light Client for Avail Blockchain

fn json_subscriber(log_level: Level) -> impl Subscriber + Send + Sync {
	FmtSubscriber::builder()
		.json()
		.with_env_filter(EnvFilter::new(format!("avail_light={log_level}")))
		.with_span_events(format::FmtSpan::CLOSE)
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

fn get_or_init_p2p_keypair(cfg: &LibP2PConfig, db: RocksDB) -> Result<identity::Keypair> {
	if let Some(secret_key) = cfg.secret_key.as_ref() {
		return p2p::keypair(secret_key);
	};

	if let Some(mut bytes) = db.get(P2PKeypairKey) {
		return Ok(ed25519::Keypair::try_from_bytes(&mut bytes[..]).map(From::from)?);
	};

	let id_keys = identity::Keypair::generate_ed25519();
	let keypair = id_keys.clone().try_into_ed25519()?;
	db.put(P2PKeypairKey, keypair.to_bytes().to_vec());
	Ok(id_keys)
}

#[cfg(not(feature = "crawl"))]
async fn run(
	cfg: RuntimeConfig,
	identity_cfg: IdentityConfig,
	db: RocksDB,
	shutdown: Controller<String>,
	client_id: Uuid,
	execution_id: Uuid,
) -> Result<()> {
	let version = clap::crate_version!();
	info!("Running Avail Light Client version: {version}.");
	info!("Using config: {cfg:?}");
	info!(
		"Avail ss58 address: {}, public key: {}",
		&identity_cfg.avail_address, &identity_cfg.avail_public_key
	);

	if cfg.libp2p.bootstraps.is_empty() {
		Err(eyre!("Bootstrap node list must not be empty. Either use a '--network' flag or add a list of bootstrap nodes in the configuration file"))?
	}

	let id_keys = get_or_init_p2p_keypair(&cfg.libp2p, db.clone())?;
	let peer_id = PeerId::from(id_keys.public()).to_string();

	let metric_attributes = MetricAttributes {
		role: "lightnode".into(),
		peer_id,
		origin: cfg.origin.clone(),
		avail_address: identity_cfg.avail_public_key.clone(),
		operating_mode: cfg.libp2p.kademlia.operation_mode.to_string(),
		partition_size: cfg
			.block_matrix_partition
			.map(|Partition { number, fraction }| format!("{number}/{fraction}"))
			.unwrap_or("n/a".to_string()),
		network: Network::name(&cfg.genesis_hash),
		version: version.to_string(),
		multiaddress: "".to_string(),
		client_id: client_id.to_string(),
		execution_id: execution_id.to_string(),
		client_alias: cfg.client_alias.clone().unwrap_or("".to_string()),
	};

	let ot_metrics = Arc::new(
		telemetry::otlp::initialize(metric_attributes, cfg.origin.clone(), cfg.otel.clone())
			.wrap_err("Unable to initialize OpenTelemetry service")?,
	);

	// Create sender channel for P2P event loop commands
	let (p2p_event_loop_sender, p2p_event_loop_receiver) = mpsc::unbounded_channel();

	let p2p_event_loop = p2p::EventLoop::new(
		cfg.libp2p.clone(),
		version,
		&cfg.genesis_hash,
		&id_keys,
		cfg.is_fat_client(),
		shutdown.clone(),
		#[cfg(feature = "kademlia-rocksdb")]
		db.inner(),
	);

	spawn_in_span(
		shutdown.with_cancel(
			p2p_event_loop
				.await
				.run(ot_metrics.clone(), p2p_event_loop_receiver),
		),
	);

	let p2p_client = p2p::Client::new(
		p2p_event_loop_sender,
		cfg.libp2p.dht_parallelization_limit,
		cfg.libp2p.kademlia.kad_record_ttl,
	);

	// Start listening on provided port
	p2p_client
		.start_listening(cfg.libp2p.multiaddress())
		.await
		.wrap_err("Listening on TCP not to fail.")?;
	info!("TCP listener started on port {}", cfg.libp2p.port);

	let p2p_clone = p2p_client.to_owned();
	let cfg_clone = cfg.to_owned();
	spawn_in_span(shutdown.with_cancel(async move {
		info!("Bootstraping the DHT with bootstrap nodes...");
		let bs_result = p2p_clone
			.bootstrap_on_startup(&cfg_clone.libp2p.bootstraps)
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
	spawn_in_span(shutdown.with_cancel(analyzer::start_traffic_analyzer(cfg.libp2p.port, 10)));

	let pp = Arc::new(kate_recovery::couscous::public_params());
	let raw_pp = pp.to_raw_var_bytes();
	let public_params_hash = hex::encode(sp_core::blake2_128(&raw_pp));
	let public_params_len = hex::encode(raw_pp).len();
	trace!("Public params ({public_params_len}): hash: {public_params_hash}");

	let (rpc_client, rpc_events, rpc_subscriptions) =
		rpc::init(db.clone(), &cfg.genesis_hash, &cfg.rpc, shutdown.clone()).await?;

	// Subscribing to RPC events before first event is published
	let publish_rpc_event_receiver = rpc_events.subscribe();
	let first_header_rpc_event_receiver = rpc_events.subscribe();
	let client_rpc_event_receiver = rpc_events.subscribe();

	// spawn the RPC Network task for Event Loop to run in the background
	// and shut it down, without delays
	let rpc_subscriptions_handle = spawn_in_span(shutdown.with_cancel(shutdown.with_trigger(
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
		.map_err(|shutdown_reason| eyre!(shutdown_reason))
		.and_then(|inner| inner)
	{
		Err(report) => {
			if !rpc_subscriptions_handle.is_finished() {
				return Err(report);
			}
			let Ok(Ok(Err(subscriptions_error))) = rpc_subscriptions_handle.await else {
				return Err(report);
			};
			return Err(eyre!(subscriptions_error));
		},
		Ok(num) => num,
	};

	db.put(LatestHeaderKey, block_header.number);
	let sync_range = cfg.sync_range(block_header.number);

	let ws_clients = api::v2::types::WsClients::default();

	// Spawn tokio task which runs one http server for handling RPC
	let server = api::server::Server {
		db: db.clone(),
		cfg: cfg.clone(),
		identity_cfg,
		version: format!("v{}", clap::crate_version!()),
		network_version: EXPECTED_SYSTEM_VERSION[0].to_string(),
		node_client: rpc_client.clone(),
		ws_clients: ws_clients.clone(),
		shutdown: shutdown.clone(),
		p2p_client: p2p_client.clone(),
	};
	spawn_in_span(shutdown.with_cancel(server.bind()));

	let (block_tx, block_rx) = broadcast::channel::<avail_light_core::types::BlockVerified>(1 << 7);

	let data_rx = cfg.app_id.map(AppId).map(|app_id| {
		let (data_tx, data_rx) = broadcast::channel::<(u32, AppData)>(1 << 7);
		spawn_in_span(shutdown.with_cancel(avail_light_core::app_client::run(
			(&cfg).into(),
			db.clone(),
			p2p_client.clone(),
			rpc_client.clone(),
			app_id,
			block_tx.subscribe(),
			pp.clone(),
			sync_range.clone(),
			data_tx,
			shutdown.clone(),
		)));
		data_rx
	});

	spawn_in_span(shutdown.with_cancel(api::v2::publish(
		api::v2::types::Topic::HeaderVerified,
		publish_rpc_event_receiver,
		ws_clients.clone(),
	)));

	spawn_in_span(shutdown.with_cancel(api::v2::publish(
		api::v2::types::Topic::ConfidenceAchieved,
		block_tx.subscribe(),
		ws_clients.clone(),
	)));

	if let Some(data_rx) = data_rx {
		spawn_in_span(shutdown.with_cancel(api::v2::publish(
			api::v2::types::Topic::DataVerified,
			data_rx,
			ws_clients,
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
		db.put(IsSyncedKey, false);
		spawn_in_span(shutdown.with_cancel(avail_light_core::sync_client::run(
			sync_client,
			sync_network_client,
			(&cfg).into(),
			sync_range,
			block_tx.clone(),
		)));
	}

	if cfg.sync_finality_enable {
		let sync_finality = SyncFinality::new(db.clone(), rpc_client.clone());
		spawn_in_span(shutdown.with_cancel(avail_light_core::sync_finality::run(
			sync_finality,
			shutdown.clone(),
			block_header.clone(),
		)));
	} else {
		warn!("Finality sync is disabled! Implicitly, blocks before LC startup will be considered verified as final");
		// set the flag in the db, signaling across that we don't need to sync
		db.put(IsFinalitySyncedKey, true);
	}

	let static_config_params: MaintenanceConfig = (&cfg).into();
	spawn_in_span(shutdown.with_cancel(avail_light_core::maintenance::run(
		p2p_client.clone(),
		ot_metrics.clone(),
		block_rx,
		static_config_params,
		shutdown.clone(),
	)));

	let channels = avail_light_core::types::ClientChannels {
		block_sender: block_tx,
		rpc_event_receiver: client_rpc_event_receiver,
	};

	if let Some(partition) = cfg.block_matrix_partition {
		let fat_client = avail_light_core::fat_client::new(p2p_client.clone(), rpc_client.clone());

		spawn_in_span(shutdown.with_cancel(avail_light_core::fat_client::run(
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

		spawn_in_span(shutdown.with_cancel(avail_light_core::light_client::run(
			db.clone(),
			light_network_client,
			(&cfg).into(),
			ot_metrics.clone(),
			channels,
			shutdown.clone(),
		)));
	}

	ot_metrics.count(MetricCounter::Starts).await;

	Ok(())
}

#[cfg(feature = "crawl")]
async fn run_crawl(
	cfg: RuntimeConfig,
	identity_cfg: IdentityConfig,
	db: RocksDB,
	shutdown: Controller<String>,
	client_id: Uuid,
	execution_id: Uuid,
) -> Result<()> {
	let version = clap::crate_version!();
	info!("Running Avail Light Client Crawler version: {version}");
	info!("Using config: {cfg:?}");
	info!(
		"Avail ss58 address: {}, public key: {}",
		&identity_cfg.avail_address, &identity_cfg.avail_public_key
	);

	if cfg.libp2p.bootstraps.is_empty() {
		Err(eyre!("Bootstrap node list must not be empty. Either use a '--network' flag or add a list of bootstrap nodes in the configuration file"))?
	}

	let id_keys = get_or_init_p2p_keypair(&cfg.libp2p, db.clone())?;
	let peer_id = PeerId::from(id_keys.public()).to_string();

	let metric_attributes = MetricAttributes {
		role: "crawler".into(),
		peer_id,
		origin: cfg.origin.clone(),
		avail_address: identity_cfg.avail_public_key.clone(),
		operating_mode: KademliaMode::Client.to_string(),
		partition_size: cfg
			.crawl
			.crawl_block_matrix_partition
			.map(|Partition { number, fraction }| format!("{number}/{fraction}"))
			.unwrap_or("n/a".to_string()),
		network: Network::name(&cfg.genesis_hash),
		version: version.to_string(),
		multiaddress: "".to_string(),
		client_id: client_id.to_string(),
		execution_id: execution_id.to_string(),
		client_alias: cfg.client_alias.clone().unwrap_or("".to_string()),
	};

	let ot_metrics = Arc::new(
		telemetry::otlp::initialize(metric_attributes, cfg.origin.clone(), cfg.otel.clone())
			.wrap_err("Unable to initialize OpenTelemetry service")?,
	);

	// Create sender channel for P2P event loop commands
	let (p2p_event_loop_sender, p2p_event_loop_receiver) = mpsc::unbounded_channel();

	let p2p_event_loop = p2p::EventLoop::new(
		cfg.libp2p.clone(),
		version,
		&cfg.genesis_hash,
		&id_keys,
		cfg.is_fat_client(),
		shutdown.clone(),
		#[cfg(feature = "kademlia-rocksdb")]
		db.inner(),
	);

	spawn_in_span(
		shutdown.with_cancel(
			p2p_event_loop
				.await
				.run(ot_metrics.clone(), p2p_event_loop_receiver),
		),
	);

	let p2p_client = p2p::Client::new(
		p2p_event_loop_sender,
		cfg.libp2p.dht_parallelization_limit,
		cfg.libp2p.kademlia.kad_record_ttl,
	);

	// Start listening on provided port
	p2p_client
		.start_listening(cfg.libp2p.multiaddress())
		.await
		.wrap_err("Listening on TCP not to fail.")?;
	info!("TCP listener started on port {}", cfg.libp2p.port);

	let p2p_clone = p2p_client.to_owned();
	let cfg_clone = cfg.to_owned();
	spawn_in_span(shutdown.with_cancel(async move {
		info!("Bootstraping the DHT with bootstrap nodes...");
		let bs_result = p2p_clone
			.bootstrap_on_startup(&cfg_clone.libp2p.bootstraps)
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

	let (_, rpc_events, rpc_subscriptions) =
		rpc::init(db.clone(), &cfg.genesis_hash, &cfg.rpc, shutdown.clone()).await?;

	// Subscribing to RPC events before first event is published
	let first_header_rpc_event_receiver = rpc_events.subscribe();
	let crawler_rpc_event_receiver = rpc_events.subscribe();

	// spawn the RPC Network task for Event Loop to run in the background
	// and shut it down, without delays
	let rpc_subscriptions_handle = spawn_in_span(shutdown.with_cancel(shutdown.with_trigger(
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
		.map_err(|shutdown_reason| eyre!(shutdown_reason))
		.and_then(|inner| inner)
	{
		Err(report) => {
			if !rpc_subscriptions_handle.is_finished() {
				return Err(report);
			}
			let Ok(Ok(Err(subscriptions_error))) = rpc_subscriptions_handle.await else {
				return Err(report);
			};
			return Err(eyre!(subscriptions_error));
		},
		Ok(num) => num,
	};

	db.put(LatestHeaderKey, block_header.number);

	let (block_tx, block_rx) = broadcast::channel::<avail_light_core::types::BlockVerified>(1 << 7);

	if cfg.crawl.crawl_block {
		let partition = cfg.crawl.crawl_block_matrix_partition;
		spawn_in_span(shutdown.with_cancel(avail_light_core::crawl_client::run(
			crawler_rpc_event_receiver,
			p2p_client.clone(),
			cfg.crawl.crawl_block_delay,
			ot_metrics.clone(),
			cfg.crawl.crawl_block_mode,
			partition.unwrap_or(avail_light_core::crawl_client::ENTIRE_BLOCK),
			block_tx,
		)));
	}

	let static_config_params: MaintenanceConfig = (&cfg).into();
	spawn_in_span(shutdown.with_cancel(avail_light_core::maintenance::run(
		p2p_client.clone(),
		ot_metrics.clone(),
		block_rx,
		static_config_params,
		shutdown.clone(),
	)));

	ot_metrics.count(MetricCounter::Starts).await;

	Ok(())
}

#[cfg(not(feature = "crawl"))]
async fn run_fat(
	cfg: RuntimeConfig,
	identity_cfg: IdentityConfig,
	db: RocksDB,
	shutdown: Controller<String>,
	client_id: Uuid,
	execution_id: Uuid,
) -> Result<()> {
	info!("Fat client mode");

	let version = clap::crate_version!();
	info!("Running Avail Light Fat Client version: {version}.");
	info!("Using config: {cfg:?}");

	if cfg.libp2p.bootstraps.is_empty() {
		Err(eyre!("Bootstrap node list must not be empty. Either use a '--network' flag or add a list of bootstrap nodes in the configuration file"))?
	}

	let id_keys = get_or_init_p2p_keypair(&cfg.libp2p, db.clone())?;
	let peer_id = PeerId::from(id_keys.public()).to_string();

	let metric_attributes = MetricAttributes {
		role: "fatnode".into(),
		peer_id,
		origin: cfg.origin.clone(),
		avail_address: identity_cfg.avail_public_key.clone(),
		operating_mode: KademliaMode::Client.to_string(),
		partition_size: cfg
			.block_matrix_partition
			.map(|Partition { number, fraction }| format!("{number}/{fraction}"))
			.unwrap_or("n/a".to_string()),
		network: Network::name(&cfg.genesis_hash),
		version: version.to_string(),
		multiaddress: "".to_string(),
		client_id: client_id.to_string(),
		execution_id: execution_id.to_string(),
		client_alias: cfg.client_alias.clone().unwrap_or("".to_string()),
	};

	let ot_metrics = Arc::new(
		telemetry::otlp::initialize(metric_attributes, cfg.origin.clone(), cfg.otel.clone())
			.wrap_err("Unable to initialize OpenTelemetry service")?,
	);

	// Create sender channel for P2P event loop commands
	let (p2p_event_loop_sender, p2p_event_loop_receiver) = mpsc::unbounded_channel();

	let p2p_event_loop = p2p::EventLoop::new(
		cfg.libp2p.clone(),
		version,
		&cfg.genesis_hash,
		&id_keys,
		cfg.is_fat_client(),
		shutdown.clone(),
		#[cfg(feature = "kademlia-rocksdb")]
		db.inner(),
	);

	spawn_in_span(
		shutdown.with_cancel(
			p2p_event_loop
				.await
				.run(ot_metrics.clone(), p2p_event_loop_receiver),
		),
	);

	let p2p_client = p2p::Client::new(
		p2p_event_loop_sender,
		cfg.libp2p.dht_parallelization_limit,
		cfg.libp2p.kademlia.kad_record_ttl,
	);

	// Start listening on provided port
	p2p_client
		.start_listening(cfg.libp2p.multiaddress())
		.await
		.wrap_err("Listening on TCP not to fail.")?;
	info!("TCP listener started on port {}", cfg.libp2p.port);

	let p2p_clone = p2p_client.to_owned();
	let cfg_clone = cfg.to_owned();
	spawn_in_span(shutdown.with_cancel(async move {
		info!("Bootstraping the DHT with bootstrap nodes...");
		let bs_result = p2p_clone
			.bootstrap_on_startup(&cfg_clone.libp2p.bootstraps)
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

	let (rpc_client, rpc_events, rpc_subscriptions) =
		rpc::init(db.clone(), &cfg.genesis_hash, &cfg.rpc, shutdown.clone()).await?;

	// Subscribing to RPC events before first event is published
	let first_header_rpc_event_receiver = rpc_events.subscribe();
	let client_rpc_event_receiver = rpc_events.subscribe();

	// spawn the RPC Network task for Event Loop to run in the background
	// and shut it down, without delays
	let rpc_subscriptions_handle = spawn_in_span(shutdown.with_cancel(shutdown.with_trigger(
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
		.map_err(|shutdown_reason| eyre!(shutdown_reason))
		.and_then(|inner| inner)
	{
		Err(report) => {
			if !rpc_subscriptions_handle.is_finished() {
				return Err(report);
			}
			let Ok(Ok(Err(subscriptions_error))) = rpc_subscriptions_handle.await else {
				return Err(report);
			};
			return Err(eyre!(subscriptions_error));
		},
		Ok(num) => num,
	};

	db.put(LatestHeaderKey, block_header.number);

	let (block_tx, block_rx) = broadcast::channel::<avail_light_core::types::BlockVerified>(1 << 7);

	if cfg.sync_finality_enable {
		let sync_finality = SyncFinality::new(db.clone(), rpc_client.clone());
		spawn_in_span(shutdown.with_cancel(avail_light_core::sync_finality::run(
			sync_finality,
			shutdown.clone(),
			block_header.clone(),
		)));
	} else {
		warn!("Finality sync is disabled! Implicitly, blocks before LC startup will be considered verified as final");
		// set the flag in the db, signaling across that we don't need to sync
		db.put(IsFinalitySyncedKey, true);
	}

	let static_config_params: MaintenanceConfig = (&cfg).into();
	spawn_in_span(shutdown.with_cancel(avail_light_core::maintenance::run(
		p2p_client.clone(),
		ot_metrics.clone(),
		block_rx,
		static_config_params,
		shutdown.clone(),
	)));

	let channels = avail_light_core::types::ClientChannels {
		block_sender: block_tx,
		rpc_event_receiver: client_rpc_event_receiver,
	};

	if let Some(partition) = cfg.block_matrix_partition {
		let fat_client = avail_light_core::fat_client::new(p2p_client.clone(), rpc_client.clone());

		spawn_in_span(shutdown.with_cancel(avail_light_core::fat_client::run(
			fat_client,
			db.clone(),
			(&cfg).into(),
			ot_metrics.clone(),
			channels,
			partition,
			shutdown.clone(),
		)));
	}

	ot_metrics.count(MetricCounter::Starts).await;

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

mod cli;

pub fn load_runtime_config(opts: &CliOpts) -> Result<RuntimeConfig> {
	let mut cfg = if let Some(config_path) = &opts.config {
		fs::metadata(config_path).map_err(|_| eyre!("Provided config file doesn't exist."))?;
		confy::load_path(config_path)
			.wrap_err(format!("Failed to load configuration from {}", config_path))?
	} else {
		RuntimeConfig::default()
	};

	cfg.log_format_json = opts.logs_json || cfg.log_format_json;

	// Flags override the config parameters
	if let Some(network) = &opts.network {
		let bootstrap: (PeerId, Multiaddr) = (
			PeerId::from_str(network.bootstrap_peer_id())
				.wrap_err("unable to parse default bootstrap peerID")?,
			Multiaddr::from_str(network.bootstrap_multiaddrr())
				.wrap_err("unable to parse default bootstrap multi-address")?,
		);
		cfg.rpc.full_node_ws = network.full_node_ws();
		cfg.libp2p.bootstraps = vec![MultiaddrConfig::PeerIdAndMultiaddr(bootstrap)];
		cfg.otel.ot_collector_endpoint = network.ot_collector_endpoint().to_string();
		cfg.genesis_hash = network.genesis_hash().to_string();
	}

	if let Some(loglvl) = &opts.verbosity {
		cfg.log_level = loglvl.to_string();
	}

	if let Some(port) = opts.port {
		cfg.libp2p.port = port;
	}
	if let Some(http_port) = opts.http_server_port {
		cfg.api.http_server_port = http_port;
	}
	if let Some(avail_path) = &opts.avail_path {
		cfg.avail_path = avail_path.to_string();
	}
	cfg.sync_finality_enable |= opts.finality_sync_enable;
	cfg.app_id = opts.app_id.or(cfg.app_id);
	cfg.libp2p.ws_transport_enable |= opts.ws_transport_enable;
	if let Some(secret_key) = &opts.private_key {
		cfg.libp2p.secret_key = Some(SecretKey::Key {
			key: secret_key.to_string(),
		});
	}

	if let Some(seed) = &opts.seed {
		cfg.libp2p.secret_key = Some(SecretKey::Seed {
			seed: seed.to_string(),
		})
	}

	if let Some(partition) = &opts.block_matrix_partition {
		cfg.block_matrix_partition = Some(*partition)
	}

	if let Some(client_alias) = &opts.client_alias {
		cfg.client_alias = Some(client_alias.clone())
	}

	Ok(cfg)
}

#[tokio::main]
pub async fn main() -> Result<()> {
	let shutdown = Controller::new();

	// install custom panic hooks
	install_panic_hooks(shutdown.clone())?;

	let opts = CliOpts::parse();

	let cfg = load_runtime_config(&opts).expect("runtime configuration is loaded");

	let (log_level, parse_error) = parse_log_level(&cfg.log_level, Level::INFO);

	if cfg.log_format_json {
		tracing::subscriber::set_global_default(json_subscriber(log_level))
			.expect("global json subscriber is set");
	} else {
		tracing::subscriber::set_global_default(default_subscriber(log_level))
			.expect("global default subscriber is set");
	};

	let suri = match opts.avail_suri {
		None => load_or_init_suri(&opts.identity)?,
		Some(suri) => suri,
	};
	let identity_cfg = IdentityConfig::from_suri(suri, opts.avail_passphrase.as_ref())?;

	if opts.clean && Path::new(&cfg.avail_path).exists() {
		info!("Cleaning up local state directory");
		fs::remove_dir_all(&cfg.avail_path).wrap_err("Failed to remove local state directory")?;
	}

	let db = RocksDB::open(&cfg.avail_path).expect("Avail Light could not initialize database");

	let client_id = db.get(ClientIdKey).unwrap_or_else(|| {
		let client_id = Uuid::new_v4();
		db.put(ClientIdKey, client_id.clone());
		client_id
	});

	let execution_id = Uuid::new_v4();

	let span = span!(
		Level::INFO,
		"run",
		client_id = client_id.to_string(),
		execution_id = execution_id.to_string(),
		client_alias = cfg.client_alias.clone().unwrap_or("".to_string())
	);
	// Do not enter span if logs format is not JSON
	let _enter = if cfg.log_format_json {
		Some(span.enter())
	} else {
		None
	};

	if let Some(error) = parse_error {
		warn!("Using default log level: {}", error);
	}

	// spawn a task to watch for ctrl-c signals from user to trigger the shutdown
	spawn_in_span(shutdown.with_trigger("user signaled shutdown".to_string(), user_signal()));

	#[cfg(feature = "crawl")]
	if let Err(error) = run_crawl(
		cfg,
		identity_cfg,
		db,
		shutdown.clone(),
		client_id,
		execution_id,
	)
	.await
	{
		error!("{error:#}");
		return Err(error.wrap_err("Starting Light Client Crawler failed"));
	};

	#[cfg(not(feature = "crawl"))]
	if let Err(error) = if cfg.is_fat_client() {
		run_fat(
			cfg,
			identity_cfg,
			db,
			shutdown.clone(),
			client_id,
			execution_id,
		)
		.await
	} else {
		run(
			cfg,
			identity_cfg,
			db,
			shutdown.clone(),
			client_id,
			execution_id,
		)
		.await
	} {
		error!("{error:#}");
		return Err(error.wrap_err("Starting Light Client failed"));
	};

	let reason = shutdown.completed_shutdown().await;

	// we are not logging error here since expectation is
	// to log terminating condition before sending message to this channel
	Err(eyre!(reason).wrap_err("Running Light Client encountered an error"))
}
