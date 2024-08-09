use avail_light_core::{
	data::RocksDB,
	network::p2p,
	shutdown::Controller,
	telemetry::otlp,
	utils::{default_subscriber, install_panic_hooks, json_subscriber, spawn_in_span},
};
use clap::Parser;
use color_eyre::{eyre::Context, Result};
use config::Config;
use std::{fs, path::Path, sync::Arc};
use tokio::sync::mpsc;
use tracing::{info, span, Level};

mod config;
mod metrics;

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

	let db = RocksDB::open(&config.avail_path)?;

	spawn_in_span(run(config, db, shutdown)).await?;

	info!("Goodbye, crawler!");
	Ok(())
}

async fn run(config: Config, db: RocksDB, shutdown: Controller<String>) -> Result<()> {
	let version = clap::crate_version!();
	info!("Running Avail Light Client Crawler v{version}");
	info!("Using configuration: {config:?}");
	info!("Hello, crawler!");

	let p2p_keypair = p2p::get_or_init_keypair(&config.libp2p, db.clone())?;

	let ot_metrics = Arc::new(
		otlp::initialize(
			metrics::Attributes::new(),
			config.origin.clone(),
			config.otel.clone(),
		)
		.wrap_err("Unable to initialize OpenTelemetry service")?,
	);

	let (p2p_event_loop_sender, p2p_event_loop_receiver) = mpsc::unbounded_channel();

	let p2p_event_loop = p2p::EventLoop::new(
		config.libp2p.clone(),
		version,
		&config.genesis_hash,
		&p2p_keypair,
		true,
		shutdown.clone(),
		db.inner(),
	);

	Ok(())
}
