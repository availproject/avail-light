#![doc = include_str!("../README.md")]

use crate::cli::CliOpts;
use avail_core::AppId;
#[cfg(feature = "memdb")]
use avail_light_core::data::MemoryDB;
#[cfg(feature = "rocksdb")]
use avail_light_core::data::RocksDB;
use avail_light_core::{
	api,
	consts::EXPECTED_SYSTEM_VERSION,
	data::{ClientIdKey, Database, IsFinalitySyncedKey, IsSyncedKey, LatestHeaderKey},
	network::{
		self,
		p2p::{self, BOOTSTRAP_LIST_EMPTY_MESSAGE},
		rpc, Network,
	},
	shutdown::Controller,
	sync_client::SyncClient,
	sync_finality::SyncFinality,
	telemetry::{self, MetricCounter, Metrics},
	types::{
		load_or_init_suri, IdentityConfig, KademliaMode, MaintenanceConfig, MultiaddrConfig,
		RuntimeConfig, SecretKey, Uuid,
	},
	utils::{default_subscriber, install_panic_hooks, json_subscriber, spawn_in_span},
};
use clap::Parser;
use color_eyre::{
	eyre::{eyre, WrapErr},
	Result,
};
use kate_recovery::com::AppData;
use kate_recovery::matrix::Partition;
use std::{fs, path::Path, sync::Arc};
use tokio::sync::{broadcast, mpsc};
use tracing::trace;
use tracing::{error, info, span, warn, Level};

#[cfg(feature = "network-analysis")]
use avail_light_core::network::p2p::analyzer;

#[cfg(all(not(target_env = "msvc"), not(target_arch = "wasm32")))]
use tikv_jemallocator::Jemalloc;

#[cfg(all(not(target_env = "msvc"), not(target_arch = "wasm32")))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

/// Light Client for Avail Blockchain

async fn run(
	cfg: RuntimeConfig,
	identity_cfg: IdentityConfig,
	db: impl Database + Clone + Send + Sync + 'static,
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

	let (id_keys, peer_id) = p2p::identity(&cfg.libp2p, db.clone())?;

	let metric_attributes = vec![
		("version", version.to_string()),
		("role", "lightnode".to_string()),
		("peerID", peer_id.to_string()),
		("avail_address", identity_cfg.avail_public_key.clone()),
		(
			"partition_size",
			cfg.block_matrix_partition
				.map(|Partition { number, fraction }| format!("{number}/{fraction}"))
				.unwrap_or("n/a".to_string()),
		),
		("network", Network::name(&cfg.genesis_hash)),
		("client_id", client_id.to_string()),
		("execution_id", execution_id.to_string()),
		(
			"client_alias",
			cfg.client_alias.clone().unwrap_or("".to_string()),
		),
	];

	let ot_metrics = Arc::new(
		telemetry::otlp::initialize(
			metric_attributes,
			&cfg.origin,
			&cfg.libp2p.kademlia.operation_mode.into(),
			cfg.otel.clone(),
		)
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
		#[cfg(feature = "rocksdb")]
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

	let addrs = vec![
		cfg.libp2p.tcp_multiaddress(),
		cfg.libp2p.webrtc_multiaddress(),
	];

	// Start the TCP and WebRTC listeners
	p2p_client
		.start_listening(addrs)
		.await
		.wrap_err("Listening on TCP not to fail.")?;
	info!(
		"TCP listener started on port {}. WebRTC listening on port {}.",
		cfg.libp2p.port, cfg.libp2p.webrtc_port
	);

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

async fn run_fat(
	cfg: RuntimeConfig,
	identity_cfg: IdentityConfig,
	db: impl Database + Clone + Send + Sync + 'static,
	shutdown: Controller<String>,
	client_id: Uuid,
	execution_id: Uuid,
) -> Result<()> {
	info!("Fat client mode");

	let version = clap::crate_version!();
	info!("Running Avail Light Fat Client version: {version}.");
	info!("Using config: {cfg:?}");

	let (id_keys, peer_id) = p2p::identity(&cfg.libp2p, db.clone())?;

	let metric_attributes = vec![
		("version", version.to_string()),
		("role", "fatnode".to_string()),
		("peerID", peer_id.to_string()),
		("avail_address", identity_cfg.avail_public_key.clone()),
		(
			"partition_size",
			cfg.block_matrix_partition
				.map(|Partition { number, fraction }| format!("{number}/{fraction}"))
				.unwrap_or("n/a".to_string()),
		),
		("network", Network::name(&cfg.genesis_hash)),
		("client_id", client_id.to_string()),
		("execution_id", execution_id.to_string()),
		(
			"client_alias",
			cfg.client_alias.clone().unwrap_or("".to_string()),
		),
	];

	let ot_metrics = Arc::new(
		telemetry::otlp::initialize(
			metric_attributes,
			&cfg.origin,
			&KademliaMode::Client.into(),
			cfg.otel.clone(),
		)
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
		#[cfg(feature = "rocksdb")]
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

	let addrs = vec![
		cfg.libp2p.tcp_multiaddress(),
		cfg.libp2p.webrtc_multiaddress(),
	];

	// Start the TCP and WebRTC listeners
	p2p_client
		.start_listening(addrs)
		.await
		.wrap_err("Listening on TCP not to fail.")?;
	info!(
		"TCP listener started on port {}. WebRTC listening on port {}.",
		cfg.libp2p.port, cfg.libp2p.webrtc_port
	);

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
	cfg.log_level = opts.verbosity.unwrap_or(cfg.log_level);

	// Flags override the config parameters
	if let Some(network) = &opts.network {
		let bootstrap = (network.bootstrap_peer_id(), network.bootstrap_multiaddr());
		cfg.rpc.full_node_ws = network.full_node_ws();
		cfg.libp2p.bootstraps = vec![MultiaddrConfig::PeerIdAndMultiaddr(bootstrap)];
		cfg.otel.ot_collector_endpoint = network.ot_collector_endpoint().to_string();
		cfg.genesis_hash = network.genesis_hash().to_string();
	}

	if let Some(port) = opts.port {
		cfg.libp2p.port = port;
	}
	if let Some(webrtc_port) = opts.webrtc_port {
		cfg.libp2p.webrtc_port = webrtc_port;
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

	if cfg.libp2p.bootstraps.is_empty() {
		return Err(eyre!("{BOOTSTRAP_LIST_EMPTY_MESSAGE}"));
	}

	Ok(cfg)
}

#[tokio::main]
pub async fn main() -> Result<()> {
	let shutdown = Controller::new();
	let opts = CliOpts::parse();
	let cfg = load_runtime_config(&opts)?;

	if cfg.log_format_json {
		tracing::subscriber::set_global_default(json_subscriber(cfg.log_level))?;
	} else {
		tracing::subscriber::set_global_default(default_subscriber(cfg.log_level))?;
	};

	// install custom panic hooks
	install_panic_hooks(shutdown.clone())?;

	let suri = match opts.avail_suri {
		None => load_or_init_suri(&opts.identity)?,
		Some(suri) => suri,
	};
	let identity_cfg = IdentityConfig::from_suri(suri, opts.avail_passphrase.as_ref())?;

	if opts.clean && Path::new(&cfg.avail_path).exists() {
		info!("Cleaning up local state directory");
		fs::remove_dir_all(&cfg.avail_path).wrap_err("Failed to remove local state directory")?;
	}

	#[cfg(feature = "rocksdb")]
	let db = RocksDB::open(&cfg.avail_path).expect("Avail Light could not initialize database");

	#[cfg(feature = "memdb")]
	let db = MemoryDB::default();

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

	// spawn a task to watch for ctrl-c signals from user to trigger the shutdown
	spawn_in_span(shutdown.on_user_signal("User signaled shutdown".to_string()));

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
