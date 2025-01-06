#![doc = include_str!("../README.md")]

use crate::cli::CliOpts;
use avail_light_core::{
	api::{self, types::ApiData},
	data::{
		self, ClientIdKey, Database, IsFinalitySyncedKey, IsSyncedKey, LatestHeaderKey,
		SignerNonceKey, DB,
	},
	light_client::{self, OutputEvent as LcEvent},
	maintenance::{self, OutputEvent as MaintenanceEvent},
	network::{
		self,
		p2p::{self, extract_block_num, OutputEvent as P2pEvent, BOOTSTRAP_LIST_EMPTY_MESSAGE},
		rpc::{self, OutputEvent as RpcEvent},
		Network,
	},
	shutdown::Controller,
	sync_client::SyncClient,
	sync_finality::SyncFinality,
	telemetry::{self, otlp::Metrics, MetricCounter, MetricValue, ATTRIBUTE_OPERATING_MODE},
	types::{
		load_or_init_suri, Delay, IdentityConfig, MaintenanceConfig, PeerAddress, SecretKey, Uuid,
	},
	utils::{default_subscriber, install_panic_hooks, json_subscriber, spawn_in_span},
};
use avail_rust::{avail_core::AppId, kate_recovery::couscous, sp_core::blake2_128};
use clap::Parser;
use color_eyre::{
	eyre::{eyre, WrapErr},
	Result,
};
use config::RuntimeConfig;
use libp2p::kad::{Mode, QueryStats, RecordKey};
use std::{collections::HashMap, fs, path::Path, sync::Arc, time::Duration};
use tokio::{
	select,
	sync::{
		broadcast,
		mpsc::{self, UnboundedReceiver},
	},
};
use tracing::{error, info, span, trace, warn, Level};

#[cfg(feature = "network-analysis")]
use avail_light_core::network::p2p::analyzer;

#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

mod cli;
mod config;

/// Light Client for Avail Blockchain
async fn run(
	cfg: RuntimeConfig,
	identity_cfg: IdentityConfig,
	db: DB,
	shutdown: Controller<String>,
	client_id: Uuid,
	execution_id: Uuid,
) -> Result<()> {
	let version = clap::crate_version!();
	let rev = env!("GIT_COMMIT_HASH");
	info!(version, rev, "Running {}", clap::crate_name!());
	info!("Using config: {cfg:?}");
	info!(
		"Avail ss58 address: {}, public key: {}",
		&identity_cfg.avail_address,
		&identity_cfg.clone().avail_public_key
	);

	let (id_keys, peer_id) = p2p::identity(&cfg.libp2p, db.clone())?;

	let (p2p_client, p2p_event_loop, p2p_event_receiver) = p2p::init(
		cfg.libp2p.clone(),
		cfg.project_name.clone(),
		id_keys,
		version,
		&cfg.genesis_hash,
		false,
		shutdown.clone(),
		#[cfg(feature = "rocksdb")]
		db.clone(),
	)
	.await?;

	spawn_in_span(shutdown.with_cancel(p2p_event_loop.run()));

	let addrs = vec![cfg.libp2p.tcp_multiaddress()];

	// Start the TCP and WebRTC listeners
	p2p_client
		.start_listening(addrs)
		.await
		.wrap_err("Error starting listener.")?;
	info!("TCP listener started on port {}", cfg.libp2p.port);

	let p2p_clone = p2p_client.to_owned();
	let cfg_clone = cfg.to_owned();
	spawn_in_span(shutdown.with_cancel(async move {
		info!("Bootstraping the DHT with bootstrap nodes...");
		if let Err(error) = p2p_clone
			.bootstrap_on_startup(&cfg_clone.libp2p.bootstraps)
			.await
		{
			warn!("Bootstrap unsuccessful: {error:#}");
		}
	}));

	#[cfg(feature = "network-analysis")]
	spawn_in_span(shutdown.with_cancel(analyzer::start_traffic_analyzer(cfg.libp2p.port, 10)));

	let pp = Arc::new(couscous::public_params());
	let raw_pp = pp.to_raw_var_bytes();
	let public_params_hash = hex::encode(blake2_128(&raw_pp));
	let public_params_len = hex::encode(raw_pp).len();
	trace!("Public params ({public_params_len}): hash: {public_params_hash}");

	let (rpc_event_sender, rpc_event_receiver) = broadcast::channel(1000);
	let (rpc_client, rpc_subscriptions) = rpc::init(
		db.clone(),
		&cfg.genesis_hash,
		&cfg.rpc,
		shutdown.clone(),
		rpc_event_sender.clone(),
	)
	.await?;

	let account_id = identity_cfg.avail_key_pair.public_key().to_account_id();
	let client = rpc_client.current_client().await;
	// let nonce = client.tx.account_nonce(&account_id).await?; TODO
	db.put(SignerNonceKey, 0);

	// Subscribing to RPC events before first event is published
	let publish_rpc_event_receiver = rpc_event_sender.subscribe();
	let first_header_rpc_event_receiver = rpc_event_sender.subscribe();
	let client_rpc_event_receiver = rpc_event_sender.subscribe();

	// spawn the RPC Network task for Event Loop to run in the background
	// and shut it down, without delays
	let rpc_subscriptions_handle = spawn_in_span(shutdown.with_cancel(shutdown.with_trigger(
		"Subscription loop failure triggered shutdown".to_string(),
		async {
			if let Err(error) = rpc_subscriptions.run().await {
				error!(%error, "Subscription loop ended with error");
			};
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
			let Ok(Err(subscriptions_error)) = rpc_subscriptions_handle.await else {
				return Err(report);
			};
			return Err(eyre!(subscriptions_error));
		},
		Ok(num) => num,
	};

	db.put(LatestHeaderKey, block_header.number);
	let sync_range = cfg.sync_range(block_header.number);

	let ws_clients = api::types::WsClients::default();

	// Spawn tokio task which runs one http server for handling RPC
	let server = api::server::Server {
		db: db.clone(),
		cfg: (&cfg).into(),
		identity_cfg: identity_cfg.clone(),
		version: format!("v{}", clap::crate_version!()),
		node_client: rpc_client.clone(),
		ws_clients: ws_clients.clone(),
		shutdown: shutdown.clone(),
		p2p_client: p2p_client.clone(),
	};
	spawn_in_span(shutdown.with_cancel(server.bind(cfg.api.clone())));

	let (block_tx, block_rx) = broadcast::channel::<avail_light_core::types::BlockVerified>(1 << 7);

	let data_rx = cfg.app_id.map(AppId).map(|app_id| {
		let (data_tx, data_rx) = broadcast::channel::<ApiData>(1 << 7);
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
		api::types::Topic::HeaderVerified,
		publish_rpc_event_receiver,
		ws_clients.clone(),
	)));

	spawn_in_span(shutdown.with_cancel(api::v2::publish(
		api::types::Topic::ConfidenceAchieved,
		block_tx.subscribe(),
		ws_clients.clone(),
	)));

	if let Some(data_rx) = data_rx {
		spawn_in_span(shutdown.with_cancel(api::v2::publish(
			api::types::Topic::DataVerified,
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
	let (maintenance_sender, maintenance_receiver) = mpsc::unbounded_channel::<MaintenanceEvent>();
	spawn_in_span(shutdown.with_cancel(maintenance::run(
		p2p_client.clone(),
		block_rx,
		static_config_params,
		shutdown.clone(),
		maintenance_sender,
	)));

	let channels = avail_light_core::types::ClientChannels {
		block_sender: block_tx,
		rpc_event_receiver: client_rpc_event_receiver,
	};

	let light_network_client = network::new(p2p_client, rpc_client, pp, cfg.disable_rpc);
	let (lc_sender, lc_receiver) = mpsc::unbounded_channel::<LcEvent>();
	spawn_in_span(shutdown.with_cancel(light_client::run(
		db.clone(),
		light_network_client,
		cfg.confidence,
		Delay(cfg.block_processing_delay),
		channels,
		shutdown.clone(),
		lc_sender,
	)));

	let operating_mode: Mode = cfg.libp2p.kademlia.operation_mode.into();

	// construct Metric Attributes and initialize Metrics
	let resource_attributes = vec![
		("version", version.to_string()),
		("role", "lightnode".to_string()),
		("origin", cfg.origin.to_string()),
		("peerID", peer_id.to_string()),
		("avail_address", identity_cfg.avail_public_key),
		("network", Network::name(&cfg.genesis_hash)),
		("client_id", client_id.to_string()),
		(
			"client_alias",
			cfg.client_alias.clone().unwrap_or("".to_string()),
		),
	];

	let mut metrics = telemetry::otlp::initialize(
		cfg.project_name.clone(),
		&cfg.origin,
		cfg.otel.clone(),
		resource_attributes,
	)
	.wrap_err("Unable to initialize OpenTelemetry service")?;

	metrics.set_attribute("execution_id", execution_id.to_string());
	metrics.set_attribute(ATTRIBUTE_OPERATING_MODE, operating_mode.to_string());

	let mut state = ClientState::new(metrics);

	spawn_in_span(shutdown.with_cancel(async move {
		state
			.handle_events(
				p2p_event_receiver,
				maintenance_receiver,
				lc_receiver,
				rpc_event_receiver,
			)
			.await;
	}));

	Ok(())
}

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
		cfg.libp2p.bootstraps = vec![PeerAddress::PeerIdAndMultiaddr(bootstrap)];
		cfg.otel.ot_collector_endpoint = network.ot_collector_endpoint().to_string();
		cfg.genesis_hash = network.genesis_hash().to_string();
	}

	if let Some(port) = opts.port {
		cfg.libp2p.port = port;
	}
	if let Some(http_port) = opts.http_server_port {
		cfg.api.http_server_port = http_port;
	}
	if let Some(webrtc_port) = opts.webrtc_port {
		cfg.libp2p.webrtc_port = webrtc_port;
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

	if let Some(client_alias) = &opts.client_alias {
		cfg.client_alias = Some(client_alias.clone())
	}

	if cfg.libp2p.bootstraps.is_empty() {
		return Err(eyre!("{BOOTSTRAP_LIST_EMPTY_MESSAGE}"));
	}

	Ok(cfg)
}

#[derive(Debug, Clone)]
struct BlockStat {
	total_count: usize,
	remaining_counter: usize,
	success_counter: usize,
	error_counter: usize,
	time_stat: u64,
}

impl BlockStat {
	fn increase_cell_counters(&mut self, cell_number: usize) {
		self.total_count += cell_number;
		self.remaining_counter += cell_number;
	}

	fn increment_success_counter(&mut self) {
		self.success_counter += 1;
	}

	fn increment_error_counter(&mut self) {
		self.error_counter += 1;
	}

	fn decrement_remaining_counter(&mut self) {
		self.remaining_counter -= 1;
	}

	fn is_completed(&self) -> bool {
		self.remaining_counter == 0
	}

	fn update_time_stat(&mut self, stats: &QueryStats) {
		self.time_stat = stats
			.duration()
			.as_ref()
			.map(Duration::as_secs)
			.unwrap_or_default();
	}

	fn success_rate(&self) -> f64 {
		self.success_counter as f64 / self.total_count as f64
	}
}

struct ClientState {
	metrics: Metrics,
	active_blocks: HashMap<u32, BlockStat>,
}

impl ClientState {
	fn new(metrics: Metrics) -> Self {
		ClientState {
			metrics,
			active_blocks: Default::default(),
		}
	}

	fn get_block_stat(&mut self, block_num: u32) -> Result<&mut BlockStat> {
		self.active_blocks
			.get_mut(&block_num)
			.ok_or_else(|| eyre!("Can't find block: {} in active block list", block_num))
	}

	fn handle_new_put_record(&mut self, block_num: u32, records: Vec<libp2p::kad::Record>) {
		self.active_blocks
			.entry(block_num)
			.and_modify(|b| b.increase_cell_counters(records.len()))
			.or_insert(BlockStat {
				total_count: records.len(),
				remaining_counter: records.len(),
				success_counter: 0,
				error_counter: 0,
				time_stat: 0,
			});
	}

	fn handle_successful_put_record(
		&mut self,
		record_key: RecordKey,
		query_stats: QueryStats,
	) -> Result<()> {
		let block_num = extract_block_num(record_key)?;
		let block = self.get_block_stat(block_num)?;

		block.increment_success_counter();
		block.decrement_remaining_counter();
		block.update_time_stat(&query_stats);

		if block.is_completed() {
			let success_rate = block.success_rate();
			let time_stat = block.time_stat as f64;

			info!(
				"Cell upload success rate for block {}: {}. Duration: {}",
				block_num, success_rate, time_stat
			);
			self.metrics
				.record(MetricValue::DHTPutSuccess(success_rate));
			self.metrics.record(MetricValue::DHTPutDuration(time_stat));
		}

		Ok(())
	}

	fn handle_failed_put_record(
		&mut self,
		record_key: RecordKey,
		query_stats: QueryStats,
	) -> Result<()> {
		let block_num = extract_block_num(record_key)?;
		let block = self.get_block_stat(block_num)?;

		block.increment_error_counter();
		block.decrement_remaining_counter();
		block.update_time_stat(&query_stats);

		if block.is_completed() {
			let success_rate = block.success_rate();
			let time_stat = block.time_stat as f64;

			info!(
				"Cell upload success rate for block {}: {}. Duration: {}",
				block_num, success_rate, time_stat
			);
			self.metrics
				.record(MetricValue::DHTPutSuccess(success_rate));
			self.metrics.record(MetricValue::DHTPutDuration(time_stat));
		}

		Ok(())
	}

	pub async fn handle_events(
		&mut self,
		mut p2p_receiver: UnboundedReceiver<P2pEvent>,
		mut maintenance_receiver: UnboundedReceiver<MaintenanceEvent>,
		mut lc_receiver: UnboundedReceiver<LcEvent>,
		mut rpc_receiver: broadcast::Receiver<RpcEvent>,
	) {
		self.metrics.count(MetricCounter::Starts);
		loop {
			select! {
					Some(p2p_event) = p2p_receiver.recv() => {
						match p2p_event {
							P2pEvent::Count => {
								self.metrics.count(MetricCounter::EventLoopEvent);
							},
							P2pEvent::IncomingGetRecord => {
								self.metrics.count(MetricCounter::IncomingGetRecord);
							},
							P2pEvent::IncomingPutRecord => {
								self.metrics.count(MetricCounter::IncomingPutRecord);
							},
							P2pEvent::KadModeChange(mode) => {
								self.metrics.set_attribute(ATTRIBUTE_OPERATING_MODE, mode.to_string());
							}
							P2pEvent::Ping(rtt) => {
								self.metrics.record(MetricValue::DHTPingLatency(rtt.as_millis() as f64));
							},
							P2pEvent::IncomingConnection => {
								self.metrics.count(MetricCounter::IncomingConnections);
							},
							P2pEvent::IncomingConnectionError => {
								self.metrics.count(MetricCounter::IncomingConnectionErrors);
							},
							P2pEvent::EstablishedConnection => {
								self.metrics.count(MetricCounter::EstablishedConnections);
							},
							P2pEvent::OutgoingConnectionError => {
								self.metrics.count(MetricCounter::OutgoingConnectionErrors);
							},
							P2pEvent::PutRecord { block_num, records } => {
								self.handle_new_put_record(block_num, records);
							},
							P2pEvent::PutRecordSuccess {
								record_key,
								query_stats,
							} => {
								if let Err(error) = self.handle_successful_put_record(record_key, query_stats){
									error!("Could not handle Successful PUT Record event properly: {error}");
								};
							},
							P2pEvent::PutRecordFailed {
								record_key,
								query_stats,
							} => {
								if let Err(error) = self.handle_failed_put_record(record_key, query_stats) {
									error!("Could not handle Failed PUT Record event properly: {error}");
								};
							},
						}
					}
				Some(maintenance_event) = maintenance_receiver.recv() => {
					match maintenance_event {
						MaintenanceEvent::RecordStats {
							connected_peers,
							block_confidence_treshold,
							replication_factor,
							query_timeout,
						} => {
							self.metrics.record(MetricValue::DHTConnectedPeers(connected_peers));
							self.metrics.record(MetricValue::BlockConfidenceThreshold(block_confidence_treshold));
							self.metrics.record(MetricValue::DHTReplicationFactor(replication_factor));
							self.metrics.record(MetricValue::DHTQueryTimeout(query_timeout));
						},
						MaintenanceEvent::CountUps => {
							self.metrics.count(MetricCounter::Up);
						},
					}
				}
				Some(lc_event) = lc_receiver.recv() => {
					match lc_event {
						LcEvent::RecordBlockProcessingDelay(delay) => {
							self.metrics.record(MetricValue::BlockProcessingDelay(delay));
						},
						LcEvent::CountSessionBlocks => {
							self.metrics.count(MetricCounter::SessionBlocks);
						},
						LcEvent::RecordBlockHeight(block_num) => {
							self.metrics.record(MetricValue::BlockHeight(block_num));
						},
						LcEvent::RecordDHTStats {
							fetched, fetched_percentage, fetch_duration
						} => {
							self.metrics.record(MetricValue::DHTFetched(fetched));
							self.metrics.record(MetricValue::DHTFetchedPercentage(fetched_percentage));
							self.metrics.record(MetricValue::DHTFetchDuration(fetch_duration));
						},
						LcEvent::RecordRPCFetched(fetched) => {
							self.metrics.record(MetricValue::RPCFetched(fetched));
						},
						LcEvent::RecordRPCFetchDuration(duration) => {
							self.metrics.record(MetricValue::RPCFetchDuration(duration));
						},
						LcEvent::RecordBlockConfidence(confidence) => {
							self.metrics.record(MetricValue::BlockConfidence(confidence));
						},
					}
				}

				Ok(_) = rpc_receiver.recv() => continue,
				// break the loop if all channels are closed
				else => break,
			}
		}
	}
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
	let identity_cfg = IdentityConfig::from_suri(suri, opts.avail_passphrase)?;

	if opts.clean && Path::new(&cfg.avail_path).exists() {
		info!("Cleaning up local state directory");
		fs::remove_dir_all(&cfg.avail_path).wrap_err("Failed to remove local state directory")?;
	}

	#[cfg(not(feature = "rocksdb"))]
	let db = data::DB::default();
	#[cfg(feature = "rocksdb")]
	let db = data::DB::open(&cfg.avail_path)?;

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

	if let Err(error) = run(
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
		return Err(error.wrap_err("Starting Light Client failed"));
	};

	let reason = shutdown.completed_shutdown().await;

	// we are not logging error here since expectation is
	// to log terminating condition before sending message to this channel
	Err(eyre!(reason).wrap_err("Running Light Client encountered an error"))
}
