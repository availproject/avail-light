#![doc = include_str!("../README.md")]

use std::time::Duration;

use avail_light_core::{
	data::{self, ClientIdKey, Database, DB},
	network::{
		p2p::{self, OutputEvent as P2pEvent},
		Network,
	},
	shutdown::Controller,
	telemetry::{self, otlp::Metrics, MetricCounter, MetricValue, ATTRIBUTE_OPERATING_MODE},
	types::{load_or_init_suri, IdentityConfig, ProjectName, Uuid},
	utils::{default_subscriber, json_subscriber, spawn_in_span},
};
use clap::Parser;
use cli::CliOpts;
use color_eyre::{
	eyre::{eyre, WrapErr},
	Result,
};
use config::RuntimeConfig;
use tokio::{
	select,
	sync::mpsc::UnboundedReceiver,
	time::{interval_at, Instant},
};
use tracing::{error, info, span, warn, Level};

mod cli;
mod config;
mod server;

const CLIENT_ROLE: &str = "bootnode";

const EXTERNAL_ADDRESS_NOT_SET_MESSAGE: &str = r#"
External address must be set for the bootstrap node. Add 'external_address' parameter in the configuration file.
"#;

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
	info!("Using config: {:?}", cfg);

	let (id_keys, peer_id) = p2p::identity(&cfg.libp2p, db.clone())?;

	let (mut p2p_client, p2p_event_loop, p2p_event_receiver) = p2p::init(
		cfg.libp2p.clone(),
		ProjectName::new("avail".to_string()),
		id_keys,
		version,
		&cfg.genesis_hash,
		true,
		shutdown.clone(),
		#[cfg(feature = "rocksdb")]
		db.clone(),
	)
	.await?;

	spawn_in_span(shutdown.with_cancel(p2p_event_loop.run()));

	p2p_client
		.start_listening(cfg.libp2p.listeners())
		.await
		.wrap_err("Error starting listener.")?;
	info!("P2P listeners started on port {}", cfg.libp2p.port);

	let p2p_clone = p2p_client.to_owned();

	if cfg.libp2p.bootstraps.is_empty() {
		info!("Running as standalone bootstrap node");
	}

	spawn_in_span(shutdown.with_cancel(async move {
		info!("Bootstraping the DHT with bootstrap nodes...");
		if let Err(error) = p2p_clone.bootstrap_on_startup().await {
			warn!("Bootstrap unsuccessful: {error:#}");
		}
	}));

	spawn_in_span(shutdown.with_cancel(server::run((&cfg).into(), p2p_client.clone())));

	let resource_attributes = vec![
		("version", version.to_string()),
		("role", CLIENT_ROLE.to_string()),
		("origin", cfg.origin.to_string()),
		("peerID", peer_id.to_string()),
		("avail_address", identity_cfg.avail_public_key),
		("network", Network::name(&cfg.genesis_hash)),
		("client_id", client_id.to_string()),
		(
			"client_alias",
			cfg.client_alias.clone().unwrap_or("".to_string()),
		),
		("revision", rev.to_string()),
	];

	let mut metrics = telemetry::otlp::initialize(
		cfg.project_name.clone(),
		&cfg.origin,
		cfg.otel.clone(),
		resource_attributes,
	)
	.wrap_err("Unable to initialize OpenTelemetry service")?;

	metrics.set_attribute("execution_id", execution_id.to_string());

	let mut state = ClientState::new(metrics, p2p_client, Duration::from_secs(20));

	spawn_in_span(shutdown.with_cancel(async move {
		state.handle_events(p2p_event_receiver).await;
	}));

	Ok(())
}

pub fn load_runtime_config(opts: &CliOpts) -> Result<RuntimeConfig> {
	let mut cfg = if let Some(cfg_path) = &opts.config {
		let raw_config = std::fs::read_to_string(cfg_path)
			.wrap_err(format!("Failed to read configuration file: {cfg_path}"))?;
		let toml_value: toml::Value = raw_config
			.parse()
			.wrap_err(format!("Failed to parse TOML in: {cfg_path}"))?;

		let mut cfg: RuntimeConfig = confy::load_path(cfg_path)
			.wrap_err(format!("Failed to load configuration from: {cfg_path}"))?;

		cfg.apply_defaults(&toml_value);

		cfg
	} else {
		RuntimeConfig::default()
	};

	cfg.apply_opts(opts);

	if cfg.libp2p.external_address.is_none() && !cfg.libp2p.local_test_mode {
		return Err(eyre!("{EXTERNAL_ADDRESS_NOT_SET_MESSAGE}"));
	}

	Ok(cfg)
}

struct ClientState {
	metrics: Metrics,
	p2p_client: p2p::Client,
	interval: Duration,
}

impl ClientState {
	fn new(metrics: Metrics, p2p_client: p2p::Client, interval: Duration) -> Self {
		ClientState {
			metrics,
			p2p_client,
			interval,
		}
	}

	pub async fn handle_events(&mut self, mut p2p_receiver: UnboundedReceiver<P2pEvent>) {
		self.metrics.count(MetricCounter::Starts);
		let mut interval = interval_at(Instant::now() + self.interval, self.interval);
		loop {
			select! {
				Some(p2p_event) = p2p_receiver.recv() => {
					match p2p_event {
						P2pEvent::Count { source, name } => {
							self.metrics.count(MetricCounter::EventLoopEvent { source, name });
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
						P2pEvent::Ping{ rtt, .. } => {
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
						_ => {}
					}
				},
				_ = interval.tick() => {
					if let Ok((peers_num, _)) = self.p2p_client.count_dht_entries().await {
						info!("Number of peers in the routing table: {}", peers_num);
						self.metrics.record(MetricValue::DHTConnectedPeers(peers_num));
					};
					self.metrics.count(MetricCounter::Up);
				},
				// break the loop if all channels are closed
				else => break,
			}
		}
	}
}

#[tokio::main]
async fn main() -> Result<()> {
	let shutdown = Controller::new();
	let opts = CliOpts::parse();
	let cfg = load_runtime_config(&opts)?;

	if cfg.log_format_json {
		tracing::subscriber::set_global_default(json_subscriber(cfg.log_level))?;
	} else {
		tracing::subscriber::set_global_default(default_subscriber(cfg.log_level))?;
	};

	#[cfg(not(target_arch = "wasm32"))]
	tracing_log::LogTracer::init()?;

	#[cfg(not(feature = "rocksdb"))]
	let db = data::DB::default();
	#[cfg(feature = "rocksdb")]
	let db = data::DB::open(&cfg.avail_bootstrap_path)?;

	let client_id = db.get(ClientIdKey).unwrap_or_else(|| {
		let client_id = Uuid::new_v4();
		db.put(ClientIdKey, client_id.clone());
		client_id
	});

	let execution_id = Uuid::new_v4();

	let suri = match opts.avail_suri {
		None => load_or_init_suri(&opts.identity)?,
		Some(suri) => suri,
	};
	let identity_cfg = IdentityConfig::from_suri(suri, opts.avail_passphrase)?;

	let span = span!(
		Level::INFO,
		"run",
		client_id = client_id.to_string(),
		execution_id = execution_id.to_string(),
		client_alias = cfg.client_alias.clone().unwrap_or("".to_string())
	); // Do not enter span if logs format is not JSON
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
		error!(%error, event_type = "BOOT_START","Bootstrap Client failed to start");
		return Err(error.wrap_err("Starting Bootstrap Client failed"));
	};

	let reason = shutdown.completed_shutdown().await;

	// we are not logging error here since expectation is
	// to log terminating condition before sending message to this channel
	Err(eyre!(reason).wrap_err("Running Bootstrap Client encountered an error"))
}
