use avail_light_core::{
	api::{self, configuration::SharedConfig},
	data::{Database, LatestHeaderKey, DB},
	fat_client::{self, OutputEvent as FatEvent},
	network::{
		p2p::{self, extract_block_num, OutputEvent as P2pEvent},
		rpc::{self, OutputEvent as RpcEvent},
		Network,
	},
	shutdown::Controller,
	telemetry::{self, otlp::Metrics, MetricCounter, MetricValue, ATTRIBUTE_OPERATING_MODE},
	types::{BlockVerified, ClientChannels, IdentityConfig, Origin, ProjectName},
	utils::{default_subscriber, install_panic_hooks, json_subscriber, spawn_in_span},
};
use clap::Parser;
use color_eyre::{
	eyre::{eyre, Context},
	Result,
};
use config::Config;
use libp2p::kad::{QueryStats, RecordKey};
use maintenance::OutputEvent as MaintenanceEvent;
use std::{collections::HashMap, fs, path::Path, time::Duration};
use tokio::{
	select,
	sync::{
		broadcast,
		mpsc::{self, UnboundedReceiver},
	},
};
use tracing::{error, info, span, warn, Level};

mod config;

fn clean_db_state(path: &str) -> Result<()> {
	if !Path::new(path).exists() {
		return Ok(());
	};
	info!("Cleaning up local state directory");
	Ok(fs::remove_dir_all(path)?)
}

#[tokio::main]
async fn main() -> Result<()> {
	let shutdown = Controller::new();
	let opts = config::CliOpts::parse();
	let config = config::load(&opts)?;

	if config.log_format_json {
		tracing::subscriber::set_global_default(json_subscriber(config.log_level))?;
	} else {
		tracing::subscriber::set_global_default(default_subscriber(config.log_level))?;
	}

	install_panic_hooks(shutdown.clone())?;

	let span = span!(Level::INFO, "run", client_alias = config.client_alias);
	let _enter = config.log_format_json.then(|| span.enter());

	spawn_in_span(shutdown.on_user_signal("User signaled shutdown".to_string()));

	if opts.clean {
		clean_db_state(&config.avail_path)?;
	};

	#[cfg(not(feature = "rocksdb"))]
	let db = DB::default();
	#[cfg(feature = "rocksdb")]
	let db = DB::open(&config.avail_path)?;

	if let Err(error) = run(config, db, shutdown.clone()).await {
		error!("{error:#}");
		return Err(error.wrap_err("Starting Fat Client failed"));
	};

	let reason = shutdown.completed_shutdown().await;

	Err(eyre!(reason).wrap_err("Running Fat Client encountered an error"))
}

async fn run(config: Config, db: DB, shutdown: Controller<String>) -> Result<()> {
	let version = clap::crate_version!();
	let rev = env!("GIT_COMMIT_HASH");
	info!(version, rev, "Running {}", clap::crate_name!());
	info!("Using configuration: {config:?}");

	let (p2p_keypair, p2p_peer_id) = p2p::identity(&config.libp2p, db.clone())?;
	let partition = config.fat.block_matrix_partition;
	let partition_size = format!("{}/{}", partition.number, partition.fraction);
	let identity_cfg = IdentityConfig::from_suri("//Alice".to_string(), None)?;

	let (p2p_client, p2p_event_loop, p2p_event_receiver) = p2p::init(
		config.libp2p.clone(),
		ProjectName::new("avail".to_string()),
		p2p_keypair,
		version,
		&config.genesis_hash,
		true,
		shutdown.clone(),
		#[cfg(feature = "rocksdb")]
		db.clone(),
	)
	.await?;

	spawn_in_span(shutdown.with_cancel(p2p_event_loop.run()));

	let addrs = vec![config.libp2p.tcp_multiaddress()];

	p2p_client
		.start_listening(addrs)
		.await
		.wrap_err("Listening on TCP not to fail.")?;
	info!("TCP listener started on port {}", config.libp2p.port);

	let bootstrap_p2p_client = p2p_client.clone();
	spawn_in_span(shutdown.with_cancel(async move {
		info!("Bootstraping the DHT with bootstrap nodes...");
		let bootstraps = &config.libp2p.bootstraps;
		if let Err(error) = bootstrap_p2p_client.bootstrap_on_startup(bootstraps).await {
			warn!("Bootstrap unsuccessful: {error:#}");
		}
	}));

	let (rpc_event_sender, rpc_event_receiver) = broadcast::channel(1000);
	let (rpc_client, rpc_subscriptions) = rpc::init(
		db.clone(),
		&config.genesis_hash,
		&config.rpc,
		shutdown.clone(),
		rpc_event_sender.clone(),
	)
	.await?;

	let first_header_rpc_event_receiver = rpc_event_sender.subscribe();
	let client_rpc_event_receiver = rpc_event_sender.subscribe();

	let rpc_subscriptions_handle = spawn_in_span(shutdown.with_cancel(shutdown.with_trigger(
		"Subscription loop failure triggered shutdown".to_string(),
		rpc_subscriptions.run(),
	)));

	info!("Waiting for first finalized header...");
	let wait_for_first_header =
		rpc::wait_for_finalized_header(first_header_rpc_event_receiver, 360);
	let block_header = match shutdown
		.with_cancel(wait_for_first_header)
		.await
		.map_err(|shutdown_reason| eyre!(shutdown_reason))
		.and_then(|inner| inner)
	{
		Err(report) if !rpc_subscriptions_handle.is_finished() => return Err(report),
		Err(report) => {
			if let Ok(Ok(Err(subscriptions_error))) = rpc_subscriptions_handle.await {
				return Err(eyre!(subscriptions_error));
			};
			return Err(report);
		},
		Ok(num) => num,
	};

	db.put(LatestHeaderKey, block_header.number);

	let (block_tx, block_rx) = broadcast::channel::<BlockVerified>(1 << 7);

	let (maintenance_sender, maintenance_receiver) = mpsc::unbounded_channel::<MaintenanceEvent>();
	spawn_in_span(shutdown.with_cancel(maintenance::run(
		block_rx,
		shutdown.clone(),
		maintenance_sender,
	)));

	let channels = ClientChannels {
		block_sender: block_tx,
		rpc_event_receiver: client_rpc_event_receiver,
	};

	let ws_clients = api::types::WsClients::default();

	let server = api::server::Server {
		db: db.clone(),
		cfg: SharedConfig::default(),
		identity_cfg,
		version: format!("v{version}"),
		node_client: rpc_client.clone(),
		ws_clients: ws_clients.clone(),
		shutdown: shutdown.clone(),
		p2p_client: p2p_client.clone(),
	};
	spawn_in_span(shutdown.with_cancel(server.bind(config.api.clone())));

	let fat_client = fat_client::new(p2p_client.clone(), rpc_client.clone());
	let (fat_sender, fat_receiver) = mpsc::unbounded_channel::<FatEvent>();
	let fat = spawn_in_span(shutdown.with_cancel(fat_client::run(
		fat_client,
		db.clone(),
		config.fat.clone(),
		config.block_processing_delay,
		fat_sender,
		channels,
		shutdown.clone(),
	)));

	let resource_attributes = vec![
		("role", "fat".to_string()),
		("origin", Origin::FatClient.to_string()),
		("version", version.to_string()),
		("peerID", p2p_peer_id.to_string()),
		("partition_size", partition_size),
		("network", Network::name(&config.genesis_hash)),
	];

	let mut metrics = telemetry::otlp::initialize(
		ProjectName::new("avail".to_string()),
		&Origin::FatClient,
		config.otel.clone(),
		resource_attributes,
	)
	.wrap_err("Unable to initialize OpenTelemetry service")?;

	metrics.set_attribute(ATTRIBUTE_OPERATING_MODE, "client".to_string());

	let mut state = FatState::new(metrics);

	spawn_in_span(shutdown.with_cancel(async move {
		state
			.handle_events(
				p2p_event_receiver,
				maintenance_receiver,
				fat_receiver,
				rpc_event_receiver,
			)
			.await;
	}));

	fat.await?.map_err(|message| eyre!(message))?;
	Ok(())
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

struct FatState {
	metrics: Metrics,
	active_blocks: HashMap<u32, BlockStat>,
}

impl FatState {
	fn new(metrics: Metrics) -> Self {
		FatState {
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

	async fn handle_events(
		&mut self,
		mut p2p_receiver: UnboundedReceiver<P2pEvent>,
		mut maintenance_receiver: UnboundedReceiver<MaintenanceEvent>,
		mut fat_receiver: UnboundedReceiver<FatEvent>,
		mut rpc_receiver: broadcast::Receiver<RpcEvent>,
	) {
		self.metrics.count(MetricCounter::Starts);
		loop {
			select! {
				Some(p2p_event) = p2p_receiver.recv() => {
					match p2p_event {
						P2pEvent::Count => {
							self.metrics.count(MetricCounter::EventLoopEvent)
						},
						P2pEvent::IncomingGetRecord => {
							self.metrics.count(MetricCounter::IncomingGetRecord)
						},
						P2pEvent::IncomingPutRecord => {
							self.metrics.count(MetricCounter::IncomingPutRecord)
						},
						P2pEvent::Ping{ rtt, .. } => {
							self.metrics.record(MetricValue::DHTPingLatency(rtt.as_millis() as f64));
						},
						P2pEvent::IncomingConnection => {
							self.metrics.count(MetricCounter::IncomingConnections)
						},
						P2pEvent::IncomingConnectionError => {
							self.metrics.count(MetricCounter::IncomingConnectionErrors)
						},
						P2pEvent::EstablishedConnection => {
							self.metrics.count(MetricCounter::EstablishedConnections)
						},
						P2pEvent::OutgoingConnectionError => {
							self.metrics.count(MetricCounter::OutgoingConnectionErrors)
						},
						P2pEvent::PutRecord { block_num, records } => {
							self.metrics.count_n(MetricCounter::DHTPutRecords, records.len() as u64);
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
						// KadModeChange Event doesn't need to be handled for Fat Clients
						_ => {}
					}
				}
				Some(maintenance_event) = maintenance_receiver.recv() => {
					match maintenance_event {
						MaintenanceEvent::CountUps => {
							self.metrics.count(MetricCounter::Up);
						},
					}
				}
				Some(fat_event) = fat_receiver.recv() => {
					match fat_event {
						FatEvent::CountSessionBlocks => {
							self.metrics.count(MetricCounter::SessionBlocks);
						},
						FatEvent::RecordBlockHeight(block_num) => {
							self.metrics.record(MetricValue::BlockHeight(block_num));
						},
						FatEvent::RecordRpcCallDuration(duration) => {
							self.metrics.record(MetricValue::RPCCallDuration(duration));
						}
						FatEvent::RecordBlockProcessingDelay(delay) => {
							self.metrics.record(MetricValue::BlockProcessingDelay(delay));
						}
					}
				}

				Ok(rpc_event) = rpc_receiver.recv() => {
					match rpc_event {
						RpcEvent::InitialConnection(_) => {
							self.metrics.count(MetricCounter::RpcConnected);
						},
						RpcEvent::SwitchedConnection(_) => {
							self.metrics.count(MetricCounter::RpcConnectionSwitched);
						}
						_ => {}
					}
				},

				// break the loop if all channels are closed
				else => break,
			}
		}
	}
}

mod maintenance {
	use avail_light_core::{shutdown::Controller, types::BlockVerified};
	use color_eyre::eyre::Report;
	use tokio::sync::{broadcast, mpsc::UnboundedSender};
	use tracing::{error, info};

	pub enum OutputEvent {
		CountUps,
	}

	pub async fn run(
		mut block_receiver: broadcast::Receiver<BlockVerified>,
		shutdown: Controller<String>,
		event_sender: UnboundedSender<OutputEvent>,
	) {
		info!("Starting maintenance...");

		loop {
			match block_receiver.recv().await.map_err(Report::from) {
				Ok(_) => {
					if let Err(error) = event_sender.send(OutputEvent::CountUps) {
						let error_msg = format!("Failed to send CountUps event: {:#}", error);
						error!("{error_msg}");
						_ = shutdown.trigger_shutdown(error_msg);
						break;
					}
				},
				Err(error) => {
					let error_msg = format!("Error receiving block: {:#}", error);
					error!("{error_msg}");
					_ = shutdown.trigger_shutdown(error_msg);
					break;
				},
			}
		}
	}
}
