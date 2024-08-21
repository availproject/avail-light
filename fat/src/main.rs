use std::{fs, path::Path, sync::Arc};

use avail_light_core::{
	data::{Database, LatestHeaderKey, RocksDB},
	fat_client,
	network::{p2p, rpc, Network},
	shutdown::Controller,
	telemetry::{self, MetricCounter, Metrics},
	types::{BlockVerified, ClientChannels, KademliaMode, Origin},
	utils::{default_subscriber, install_panic_hooks, json_subscriber, spawn_in_span},
};
use clap::Parser;
use color_eyre::{
	eyre::{eyre, Context},
	Result,
};
use config::Config;
use tokio::sync::broadcast;
use tracing::{info, span, warn, Level};

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

	let db = RocksDB::open(&config.avail_path)?;

	let _ = spawn_in_span(run(config, db, shutdown)).await?;
	Ok(())
}

async fn run(config: Config, db: RocksDB, shutdown: Controller<String>) -> Result<()> {
	let version = clap::crate_version!();
	info!("Running Avail Light Fat Client v{version}");
	info!("Using configuration: {config:?}");

	let (p2p_keypair, p2p_peer_id) = p2p::identity(&config.libp2p, db.clone())?;
	let partition = config.fat.block_matrix_partition;
	let partition_size = format!("{}/{}", partition.number, partition.fraction);

	let metric_attributes = vec![
		("role", "fat".to_string()),
		("version", version.to_string()),
		("peerID", p2p_peer_id.to_string()),
		("partition_size", partition_size),
		("network", Network::name(&config.genesis_hash)),
		("client_alias", config.client_alias.clone()),
	];

	let ot_metrics = Arc::new(
		telemetry::otlp::initialize(
			metric_attributes,
			&Origin::FatClient,
			&KademliaMode::Client.into(),
			config.otel.clone(),
		)
		.wrap_err("Unable to initialize OpenTelemetry service")?,
	);

	let (p2p_client, p2p_event_loop, _) = p2p::init(
		config.libp2p.clone(),
		p2p_keypair,
		version,
		&config.genesis_hash,
		true,
		shutdown.clone(),
		db.inner(),
	)
	.await?;

	spawn_in_span(shutdown.with_cancel(p2p_event_loop.run(ot_metrics.clone())));

	let addrs = vec![
		config.libp2p.tcp_multiaddress(),
		config.libp2p.webrtc_multiaddress(),
	];

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

	spawn_in_span(shutdown.with_cancel(maintenance::run(
		config.otel.ot_flush_block_interval,
		block_rx,
		shutdown.clone(),
		ot_metrics.clone(),
	)));

	let channels = ClientChannels {
		block_sender: block_tx,
		rpc_event_receiver: client_rpc_event_receiver,
	};

	let fat_client = fat_client::new(p2p_client.clone(), rpc_client.clone());

	let fat = spawn_in_span(shutdown.with_cancel(fat_client::run(
		fat_client,
		db.clone(),
		config.fat.clone(),
		config.block_processing_delay,
		ot_metrics.clone(),
		channels,
		shutdown.clone(),
	)));

	ot_metrics.count(MetricCounter::Starts).await;
	fat.await?.map_err(|message| eyre!(message))?;
	Ok(())
}

mod maintenance {
	use std::sync::Arc;

	use avail_light_core::{
		shutdown::Controller,
		telemetry::{MetricCounter, Metrics},
		types::BlockVerified,
	};
	use color_eyre::eyre::Report;
	use tokio::sync::broadcast;
	use tracing::{error, info};

	pub async fn run(
		ot_flush_block_interval: u32,
		mut block_receiver: broadcast::Receiver<BlockVerified>,
		shutdown: Controller<String>,
		metrics: Arc<impl Metrics>,
	) {
		info!("Starting maintenance...");

		loop {
			match block_receiver.recv().await.map_err(Report::from) {
				Ok(block) => {
					let block_num = block.block_num;
					if block_num % ot_flush_block_interval == 0 {
						info!(block_num, "Flushing metrics...");
						if let Err(error) = metrics.flush().await {
							error!(block_num, "Flushing metrics failed: {error:#}");
						} else {
							info!(block_num, "Flushing metrics finished");
						}
					};
					metrics.count(MetricCounter::Up).await;
				},
				Err(error) => {
					_ = shutdown.trigger_shutdown(format!("{error:#}"));
					break;
				},
			}
		}
	}
}
