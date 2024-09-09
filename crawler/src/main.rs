use avail_light_core::{
	crawl_client::{self, CrawlMetricValue, OutputEvent as CrawlerEvent},
	data::{Database, LatestHeaderKey, DB},
	network::{
		p2p::{self, extract_block_num, OutputEvent as P2pEvent},
		rpc, Network,
	},
	shutdown::Controller,
	telemetry::{
		otlp::{self, Metrics},
		MetricCounter, MetricValue,
	},
	types::BlockVerified,
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
pub async fn main() -> Result<()> {
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

	let _ = spawn_in_span(run(config, db, shutdown)).await?;

	Ok(())
}

async fn run(config: Config, db: DB, shutdown: Controller<String>) -> Result<()> {
	let version = clap::crate_version!();
	info!("Running Avail Light Client Crawler v{version}");
	info!("Using configuration: {config:?}");

	let (p2p_keypair, p2p_peer_id) = p2p::identity(&config.libp2p, db.clone())?;
	let partition = config.crawl_block_matrix_partition;
	let partition_size = format!("{}/{}", partition.number, partition.fraction);

	let metrics = otlp::initialize("avail".to_string(), &config.origin, config.otel.clone())
		.wrap_err("Unable to initialize OpenTelemetry service")?;

	let (p2p_client, p2p_event_loop, p2p_event_receiver) = p2p::init(
		config.libp2p.clone(),
		"avail".to_string(),
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

	p2p_client
		.start_listening(vec![config.libp2p.tcp_multiaddress()])
		.await
		.wrap_err("Error starting listeners.")?;
	info!("TCP listener started on port {}", config.libp2p.port);

	let bootstrap_p2p_client = p2p_client.clone();
	spawn_in_span(shutdown.with_cancel(async move {
		info!("Bootstraping the DHT with bootstrap nodes...");
		let bootstraps = &config.libp2p.bootstraps;
		match bootstrap_p2p_client.bootstrap_on_startup(bootstraps).await {
			Ok(()) => info!("Bootstrap done."),
			Err(e) => warn!("Bootstrap error: {e:?}."),
		}
	}));

	let (_, rpc_events, rpc_subscriptions) = rpc::init(
		db.clone(),
		&config.genesis_hash,
		&config.rpc,
		shutdown.clone(),
	)
	.await?;

	let first_header_rpc_event_receiver = rpc_events.subscribe();
	let client_rpc_event_receiver = rpc_events.subscribe();

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
		config.otel.ot_flush_block_interval,
		block_rx,
		shutdown.clone(),
		maintenance_sender,
	)));

	let (crawler_sender, crawler_receiver) = mpsc::unbounded_channel::<CrawlerEvent>();
	let crawler = spawn_in_span(shutdown.with_cancel(crawl_client::run(
		client_rpc_event_receiver,
		p2p_client.clone(),
		config.crawl_block_delay,
		config.crawl_block_mode,
		partition,
		block_tx,
		crawler_sender,
	)));

	let metric_attributes = vec![
		("role".to_string(), "crawler".to_string()),
		("version".to_string(), version.to_string()),
		("peerID".to_string(), p2p_peer_id.to_string()),
		("partition_size".to_string(), partition_size),
		("network".to_string(), Network::name(&config.genesis_hash)),
		("client_alias".to_string(), config.client_alias),
		("operating_mode".to_string(), "client".to_string()),
	];

	let mut state = CrawlerState::new(metrics, String::default(), metric_attributes);

	spawn_in_span(shutdown.with_cancel(async move {
		state
			.handle_events(p2p_event_receiver, maintenance_receiver, crawler_receiver)
			.await;
	}));

	crawler.await?.map_err(|message| eyre!(message))?;
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

struct CrawlerState {
	metrics: Metrics,
	multiaddress: String,
	metric_attributes: Vec<(String, String)>,
	active_blocks: HashMap<u32, BlockStat>,
}

impl CrawlerState {
	fn new(
		metrics: Metrics,
		multiaddress: String,
		metric_attributes: Vec<(String, String)>,
	) -> Self {
		CrawlerState {
			metrics,
			multiaddress,
			metric_attributes,
			active_blocks: Default::default(),
		}
	}

	fn update_multiaddress(&mut self, value: String) {
		self.multiaddress = value;
	}

	fn attributes(&self) -> Vec<(String, String)> {
		let mut attrs = vec![("multiaddress".to_string(), self.multiaddress.clone())];

		attrs.extend(self.metric_attributes.clone());
		attrs
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
		mut crawler_receiver: UnboundedReceiver<CrawlerEvent>,
	) {
		self.metrics.count(MetricCounter::Starts, self.attributes());
		loop {
			select! {
				Some(p2p_event) = p2p_receiver.recv() => {
					match p2p_event {
						P2pEvent::Count => {
							self.metrics.count(MetricCounter::EventLoopEvent, self.attributes());
						},
						P2pEvent::IncomingGetRecord => {
							self.metrics.count(MetricCounter::IncomingGetRecord, self.attributes());
						},
						P2pEvent::IncomingPutRecord => {
							self.metrics.count(MetricCounter::IncomingPutRecord, self.attributes());
						},
						P2pEvent::Ping(rtt) => {
							self.metrics.record(MetricValue::DHTPingLatency(rtt.as_millis() as f64))
								;
						},
						P2pEvent::IncomingConnection => {
							self.metrics.count(MetricCounter::IncomingConnections, self.attributes());
						},
						P2pEvent::IncomingConnectionError => {
							self.metrics.count(MetricCounter::IncomingConnectionErrors, self.attributes());
						},
						P2pEvent::MultiaddressUpdate(address) => {
							self.update_multiaddress(address.to_string());
						},
						P2pEvent::EstablishedConnection => {
							self.metrics.count(MetricCounter::EstablishedConnections, self.attributes());
						},
						P2pEvent::OutgoingConnectionError => {
							self.metrics.count(MetricCounter::OutgoingConnectionErrors, self.attributes());
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
						// KadModeChange Event doesn't need to be handled for Crawler Clients
						_ => {}
					}
				}
				Some(maintenance_event) = maintenance_receiver.recv() => {
					match maintenance_event {
						MaintenanceEvent::FlushMetrics(block_num) => {
							if let Err(error) = self.metrics.flush(self.attributes()) {
								error!(
									block_num,
									"Could not handle Flush Maintenance event properly: {error}"
								);
							} else {
								info!(block_num, "Flushing metrics finished");
							};
						},
						MaintenanceEvent::CountUps => {
							self.metrics.count(MetricCounter::Up, self.attributes());
						},
					}
				}
				Some(crawler_event) = crawler_receiver.recv() => {
					match crawler_event {
						CrawlerEvent::RecordBlockDelay(delay) => {
							self.metrics.record(CrawlMetricValue::BlockDelay(delay));
						},
						CrawlerEvent::RecordCellSuccessRate(success_rate)=> {
							self.metrics.record(CrawlMetricValue::CellsSuccessRate(success_rate));

						}
						CrawlerEvent::RecordRowsSuccessRate(success_rate) => {
							self.metrics.record(CrawlMetricValue::RowsSuccessRate(success_rate));
						}
					}
				}
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
		FlushMetrics(u32),
		CountUps,
	}

	pub async fn run(
		ot_flush_block_interval: u32,
		mut block_receiver: broadcast::Receiver<BlockVerified>,
		shutdown: Controller<String>,
		event_sender: UnboundedSender<OutputEvent>,
	) {
		info!("Starting maintenance...");

		loop {
			match block_receiver.recv().await.map_err(Report::from) {
				Ok(block) => {
					let block_num = block.block_num;
					if block_num % ot_flush_block_interval == 0 {
						info!(block_num, "Flushing metrics...");
						if let Err(error) = event_sender.send(OutputEvent::FlushMetrics(block_num))
						{
							let error_msg =
								format!("Failed to send FlushMetrics event: {:#}", error);
							error!("{error_msg}");
							_ = shutdown.trigger_shutdown(error_msg);
							break;
						}
					};
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
