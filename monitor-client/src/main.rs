use avail_light_core::{
	data,
	network::p2p::{self, configuration::LibP2PConfig},
	shutdown::Controller,
	types::ProjectName,
	utils::spawn_in_span,
};
use clap::Parser;
use color_eyre::{eyre::Context, Result};
use tracing::info;

mod config;

#[tokio::main]
async fn main() -> Result<()> {
	info!("Starting Avail monitors...");
	let version = clap::crate_version!();

	let opts = config::CliOpts::parse();

	let db = data::DB::open(&opts.db_path)?;

	let config = config::load(&opts)?;
	let shutdown = Controller::new();

	let (p2p_keypair, p2p_peer_id) = p2p::identity(&config.libp2p, db.clone())?;

	let (p2p_client, p2p_event_loop, p2p_event_receiver) = p2p::init(
		config.libp2p.clone(),
		ProjectName::new("avail".to_string()),
		p2p_keypair,
		version,
		opts.network.genesis_hash(),
		true,
		shutdown.clone(),
		db.clone(),
	)
	.await?;

	spawn_in_span(shutdown.with_cancel(p2p_event_loop.run()));

	let addrs = vec![config.libp2p.tcp_multiaddress()];

	p2p_client
		.start_listening(addrs)
		.await
		.wrap_err("Error starting listener.")?;
	info!("TCP listener started on port {}", config.libp2p.port);

	Ok(())
}
