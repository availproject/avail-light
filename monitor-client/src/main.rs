use std::time::Duration;

use bootstrap_monitor::BootstrapMonitor;
use tokio::time;

use avail_light_core::{
	data,
	network::p2p::{self},
	shutdown::Controller,
	types::ProjectName,
	utils::{default_subscriber, json_subscriber, spawn_in_span},
};
use clap::Parser;
use color_eyre::{eyre::Context, Result};
use tracing::{error, info};

mod bootstrap_monitor;
mod config;

#[tokio::main]
async fn main() -> Result<()> {
	info!("Starting Avail monitors...");
	let version = clap::crate_version!();

	let opts = config::CliOpts::parse();
	let config = config::load(&opts)?;

	if config.log_format_json {
		tracing::subscriber::set_global_default(json_subscriber(config.log_level))?;
	} else {
		tracing::subscriber::set_global_default(default_subscriber(config.log_level))?;
	}
	info!("Using configuration: {config:?}");

	#[cfg(not(feature = "rocksdb"))]
	let db = data::DB::default();
	#[cfg(feature = "rocksdb")]
	let db = data::DB::open(&config.db_path)?;

	let shutdown = Controller::new();

	let (p2p_keypair, _) = p2p::identity(&config.libp2p, db.clone())?;

	let (p2p_client, p2p_event_loop, _) = p2p::init(
		config.libp2p.clone(),
		ProjectName::new("avail".to_string()),
		p2p_keypair,
		version,
		&config.genesis_hash,
		true,
		shutdown.clone(),
		db.clone(),
	)
	.await?;

	info!("Starting event loop");

	spawn_in_span(shutdown.with_cancel(p2p_event_loop.run()));

	let addrs = vec![config.libp2p.tcp_multiaddress()];

	p2p_client
		.start_listening(addrs)
		.await
		.wrap_err("Error starting listener.")?;
	info!("TCP listener started on port {}", config.libp2p.port);

	let interval = time::interval(Duration::from_secs(config.interval));

	// 1. Test bootstrap availability

	let mut bootstrap_monitor =
		BootstrapMonitor::new(config.libp2p.bootstraps, interval, p2p_client);
	_ = spawn_in_span(shutdown.with_cancel(async move {
		if let Err(e) = bootstrap_monitor.start_monitoring().await {
			error!("Bootstrap monitor error: {e}");
		};
	}))
	.await?;

	// 2. Test the number of discovered clients from the bootstrap
	// 3. Test server nodes availability
	// 4. Test server latencies

	Ok(())
}
