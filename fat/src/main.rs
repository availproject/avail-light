use avail_light_core::{
	api::{self, configuration::SharedConfig},
	consts::EXPECTED_SYSTEM_VERSION,
	data::{Database, LatestHeaderKey, DB},
	fat_client::{self, OutputEvent as FatEvent},
	network::{
		p2p::{self, OutputEvent as P2pEvent},
		rpc, Network,
	},
	shutdown::Controller,
	telemetry::{self, MetricCounter, MetricValue, Metrics},
	types::{BlockVerified, ClientChannels, IdentityConfig, KademliaMode, Origin},
	utils::{default_subscriber, install_panic_hooks, json_subscriber, spawn_in_span},
};
use clap::Parser;
use color_eyre::{
	eyre::{eyre, Context},
	Result,
};
use config::Config;
use maintenance::OutputEvent as MaintenanceEvent;
use std::{fs, path::Path};
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

	let _ = spawn_in_span(run(config, db, shutdown)).await?;
	Ok(())
}

async fn run(config: Config, db: DB, shutdown: Controller<String>) -> Result<()> {
	let version = clap::crate_version!();
	info!("Running Avail Light Fat Client v{version}");
	info!("Using configuration: {config:?}");

	let (p2p_keypair, p2p_peer_id) = p2p::identity(&config.libp2p, db.clone())?;
	let partition = config.fat.block_matrix_partition;
	let partition_size = format!("{}/{}", partition.number, partition.fraction);

	// TODO: Remove once the P2P API is decoupled
	let identity_cfg = IdentityConfig::from_suri("//Alice".to_string(), None)?;

	let metric_attributes = vec![
		("role", "fat".to_string()),
		("version", version.to_string()),
		("peerID", p2p_peer_id.to_string()),
		("partition_size", partition_size),
		("network", Network::name(&config.genesis_hash)),
		("client_alias", config.client_alias.clone()),
	];

	let mut metrics = telemetry::otlp::initialize(
		metric_attributes,
		"avail".to_string(),
		&Origin::FatClient,
		KademliaMode::Client.into(),
		config.otel.clone(),
	)
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
		match bootstrap_p2p_client.bootstrap_on_startup(bootstraps).await {
			Ok(()) => info!("Bootstrap done."),
			Err(e) => warn!("Bootstrap error: {e:?}."),
		}
	}));

	let (rpc_client, rpc_events, rpc_subscriptions) = rpc::init(
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

	let channels = ClientChannels {
		block_sender: block_tx,
		rpc_event_receiver: client_rpc_event_receiver,
	};

	let ws_clients = api::v2::types::WsClients::default();

	let server = api::server::Server {
		db: db.clone(),
		cfg: SharedConfig::default(),
		identity_cfg,
		version: format!("v{}", clap::crate_version!()),
		network_version: EXPECTED_SYSTEM_VERSION[0].to_string(),
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

	metrics.count(MetricCounter::Starts);
	spawn_in_span(shutdown.with_cancel(handle_events(
		metrics,
		p2p_event_receiver,
		maintenance_receiver,
		fat_receiver,
	)));

	fat.await?.map_err(|message| eyre!(message))?;
	Ok(())
}

async fn handle_events(
	mut metrics: impl Metrics,
	mut p2p_receiver: UnboundedReceiver<P2pEvent>,
	mut maintenance_receiver: UnboundedReceiver<MaintenanceEvent>,
	mut fat_receiver: UnboundedReceiver<FatEvent>,
) {
	loop {
		select! {
			Some(p2p_event) = p2p_receiver.recv() => {
				match p2p_event {
					P2pEvent::Count => {
						metrics.count(MetricCounter::EventLoopEvent);
					},
					P2pEvent::IncomingGetRecord => {
						metrics.count(MetricCounter::IncomingGetRecord);
					},
					P2pEvent::IncomingPutRecord => {
						metrics.count(MetricCounter::IncomingPutRecord);
					},
					P2pEvent::KadModeChange(mode) => {
						metrics.update_operating_mode(mode);
					},
					P2pEvent::Ping(rtt) => {
						metrics.record(MetricValue::DHTPingLatency(rtt.as_millis() as f64))
							;
					},
					P2pEvent::IncomingConnection => {
						metrics.count(MetricCounter::IncomingConnections);
					},
					P2pEvent::IncomingConnectionError => {
						metrics.count(MetricCounter::IncomingConnectionErrors);
					},
					P2pEvent::MultiaddressUpdate(address) => {
						metrics.update_multiaddress(address);
					},
					P2pEvent::EstablishedConnection => {
						metrics.count(MetricCounter::EstablishedConnections);
					},
					P2pEvent::OutgoingConnectionError => {
						metrics.count(MetricCounter::OutgoingConnectionErrors);
					},
					P2pEvent::PutRecord { block_num, records } => {
						metrics.handle_new_put_record(block_num, records);
					},
					P2pEvent::PutRecordSuccess {
						record_key,
						query_stats,
					} => {
						if let Err(error) = metrics.handle_successful_put_record(record_key, query_stats){
							error!("Could not handle Successful PUT Record event properly: {error}");
						};
					},
					P2pEvent::PutRecordFailed {
						record_key,
						query_stats,
					} => {
						if let Err(error) = metrics.handle_failed_put_record(record_key, query_stats) {
							error!("Could not handle Failed PUT Record event properly: {error}");
						};
					},
				}
			}
			Some(maintenance_event) = maintenance_receiver.recv() => {
				match maintenance_event {
					MaintenanceEvent::FlushMetrics(block_num) => {
						if let Err(error) = metrics.flush() {
							error!(
								block_num,
								"Could not handle Flush Maintenance event properly: {error}"
							);
						} else {
							info!(block_num, "Flushing metrics finished");
						};
					},
					MaintenanceEvent::CountUps => {
						metrics.count(MetricCounter::Up);
					},
				}
			}
			Some(fat_event) = fat_receiver.recv() => {
				match fat_event {
					FatEvent::CountSessionBlocks => {
						metrics.count(MetricCounter::SessionBlocks);
					},
					FatEvent::RecordBlockHeight(block_num) => {
						metrics.record(MetricValue::BlockHeight(block_num));
					},
					FatEvent::RecordRpcCallDuration(duration) => {
						metrics.record(MetricValue::RPCCallDuration(duration));
					}
					FatEvent::RecordBlockProcessingDelay(delay) => {
						metrics.record(MetricValue::BlockProcessingDelay(delay));
					}
				}
			}
			// break the loop if all channels are closed
			else => break,
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
