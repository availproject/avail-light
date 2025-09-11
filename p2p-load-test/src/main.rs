#[cfg(feature = "multiproof")]
use avail_core::data_proof::BoundedData;
use avail_light_core::{
	data,
	network::p2p::{self, Client as P2pClient, OutputEvent},
	shutdown::Controller,
	types::ProjectName,
	utils::{default_subscriber, install_panic_hooks, json_subscriber, spawn_in_span},
};
use clap::Parser;
use color_eyre::{eyre::Context, Result};
use kate_recovery::{data::Cell, matrix::Position};
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::{
	sync::Arc,
	time::{Duration, Instant},
};
use tokio::{
	select,
	sync::{mpsc::UnboundedReceiver, Mutex},
	time::{self, sleep},
};
use tracing::{debug, error, info, trace, warn};

mod config;

#[cfg(feature = "multiproof")]
use kate_recovery::data::MultiProofCell;
#[cfg(not(feature = "multiproof"))]
use kate_recovery::data::SingleCell;

struct LoadTestState {
	blocks_sent: u64,
	cells_sent: u64,
	errors: u64,
	start_time: Instant,
}

impl LoadTestState {
	fn new() -> Self {
		Self {
			blocks_sent: 0,
			cells_sent: 0,
			errors: 0,
			start_time: Instant::now(),
		}
	}

	fn print_stats(&self) {
		let elapsed = self.start_time.elapsed().as_secs();
		if elapsed > 0 {
			let blocks_per_sec = self.blocks_sent as f64 / elapsed as f64;
			let cells_per_sec = self.cells_sent as f64 / elapsed as f64;
			info!(
                "Stats - Blocks sent: {}, Cells sent: {}, Errors: {}, Blocks/sec: {:.2}, Cells/sec: {:.2}",
                self.blocks_sent, self.cells_sent, self.errors, blocks_per_sec, cells_per_sec
            );
		}
	}
}

fn generate_random_cell(rng: &mut StdRng, position: Position) -> Cell {
	#[cfg(feature = "multiproof")]
	{
		let mut data = vec![0u8; 2048]; // 2KB for multiproof cells
		rng.fill(&mut data[..]);
		let bounded_data = BoundedData::try_from(data).expect("Data size should be valid");

		Cell::MultiProofCell(MultiProofCell {
			position,
			data: bounded_data,
			proof: vec![], // Empty proof for load testing
		})
	}
	#[cfg(not(feature = "multiproof"))]
	{
		let mut content = [0u8; 80]; // SingleCell has fixed size content
		rng.fill(&mut content[..]);

		Cell::SingleCell(SingleCell { position, content })
	}
}

fn generate_block_cells(block_num: u32, cell_count: u16, seed: u64) -> Vec<Cell> {
	let mut rng = StdRng::seed_from_u64(seed + block_num as u64);
	let mut cells = Vec::with_capacity(cell_count as usize);

	for col in 0..cell_count {
		let position = Position { row: 0, col };
		cells.push(generate_random_cell(&mut rng, position));
	}

	cells
}

async fn run_load_test(
	p2p_client: P2pClient,
	config: config::Config,
	shutdown: Controller<String>,
) -> Result<()> {
	let state = Arc::new(Mutex::new(LoadTestState::new()));
	let state_clone = state.clone();

	// Stats printer task
	spawn_in_span(shutdown.with_cancel(async move {
		let mut interval = time::interval(Duration::from_secs(10));
		loop {
			interval.tick().await;
			let state = state_clone.lock().await;
			state.print_stats();
		}
	}));

	// Calculate cells per block based on block size
	#[cfg(feature = "multiproof")]
	let cell_size = 2048; // 2KB per multiproof cell
	#[cfg(not(feature = "multiproof"))]
	let cell_size = 80; // 80 bytes per single cell

	let cells_per_block = (config.block_size / cell_size).min(256) as u16;

	info!(
        "Starting load test - Rate: {} blocks/sec, Block size: {} bytes, Cells per block: {}, Duration: {} sec{}",
        config.rate, config.block_size, cells_per_block, config.duration,
        if config.target_peers.is_empty() {
            "".to_string()
        } else {
            format!(", Targeting {} specific peers", config.target_peers.len())
        }
    );

	// Pre-generate blocks
	let mut blocks = Vec::with_capacity(config.block_count as usize);
	for i in 0..config.block_count {
		let cells = generate_block_cells(i, cells_per_block, 42); // Fixed seed for reproducibility
		blocks.push(cells);
	}
	info!("Pre-generated {} blocks", blocks.len());

	let interval_ms = 1000 / config.rate;
	let mut interval = time::interval(Duration::from_millis(interval_ms));
	let start_time = Instant::now();
	let mut block_counter = 0u32;

	loop {
		if config.duration > 0 && start_time.elapsed().as_secs() >= config.duration {
			info!("Test duration reached, stopping...");
			break;
		}

		select! {
			_ = interval.tick() => {
				let block_idx = (block_counter % config.block_count) as usize;
				let cells = blocks[block_idx].clone();
				let cell_count = cells.len();

				let result = if config.target_peers.is_empty() {
					// Use standard DHT insertion
					p2p_client.insert_cells_into_dht(block_counter, cells).await
				} else {
					// Use targeted DHT insertion
					p2p_client
						.insert_cells_into_dht_to(block_counter, cells, config.target_peers.clone())
						.await
				};

				match result {
					Ok(_) => {
						let mut state = state.lock().await;
						state.blocks_sent += 1;
						state.cells_sent += cell_count as u64;
					}
					Err(e) => {
						warn!("Failed to insert cells for block {}: {}", block_counter, e);
						let mut state = state.lock().await;
						state.errors += 1;
					}
				}

				block_counter += 1;
			}
			_ = shutdown.triggered_shutdown() => {
				info!("Shutdown signal received, stopping load test...");
				break;
			}
		}
	}

	// Print final stats
	let state = state.lock().await;
	info!("Load test completed!");
	state.print_stats();

	Ok(())
}

async fn handle_events(mut p2p_receiver: UnboundedReceiver<OutputEvent>) -> Result<()> {
	loop {
		select! {
			Some(p2p_event) = p2p_receiver.recv() => {
				match p2p_event {
					OutputEvent::PutRecord { block_num, records } => {
						trace!("Put {} records for block {}", records.len(), block_num);
					}
					OutputEvent::PutRecordSuccess { record_key, query_stats } => {
						debug!("Successfully put record: {:?}, stats: {:?}", record_key, query_stats);
					}
					OutputEvent::PutRecordFailed { record_key, query_stats } => {
						warn!("Failed to put record: {:?}, stats: {:?}", record_key, query_stats);
					}
					_ => {}
				}
			}
			else => {
				info!("Event channel closed, exiting event handler");
				break;
			}
		}
	}
	Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
	let version = clap::crate_version!();
	let opts = config::CliOpts::parse();
	let config = config::load(&opts)?;

	if config.log_format_json {
		tracing::subscriber::set_global_default(json_subscriber(config.log_level))?;
	} else {
		tracing::subscriber::set_global_default(default_subscriber(config.log_level))?;
	};
	info!("Starting Avail P2P Load Test Client...");

	#[cfg(not(feature = "rocksdb"))]
	let db = data::DB::default();
	#[cfg(feature = "rocksdb")]
	let db = data::DB::open(&config.db_path)?;

	let shutdown = Controller::new();
	install_panic_hooks(shutdown.clone())?;

	info!("Using configuration: {config:?}");

	let (p2p_keypair, _) = p2p::identity(&config.libp2p, db.clone())?;

	let (mut p2p_client, p2p_event_loop, p2p_events) = p2p::init(
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

	p2p_client
		.start_listening(config.libp2p.listeners())
		.await
		.wrap_err("Error starting listener.")?;
	info!("P2P listener started on port {}", config.libp2p.port);

	// Bootstrap
	p2p_client.bootstrap_on_startup().await?;
	info!("Connected to bootstrap nodes");

	// Add manual peers to routing table
	for peer_addr in &config.manual_peers {
		let (peer_id, multiaddr) = peer_addr.into();
		match p2p_client.add_address(peer_id, multiaddr.clone()).await {
			Ok(_) => info!("Added manual peer {} to routing table", peer_id),
			Err(e) => warn!("Failed to add manual peer {}: {}", peer_id, e),
		}
	}
	if !config.manual_peers.is_empty() {
		info!(
			"Added {} manual peers to routing table",
			config.manual_peers.len()
		);
	}

	// Start event handler
	spawn_in_span(shutdown.with_cancel(async move {
		if let Err(e) = handle_events(p2p_events).await {
			error!("Event handler error: {e}");
		}
	}));

	// Wait a bit for initial peer discovery
	sleep(Duration::from_secs(config.peer_discovery_interval)).await;

	// Run the load test
	run_load_test(p2p_client, config, shutdown.clone()).await?;

	Ok(())
}
