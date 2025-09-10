use crate::server::{get_blacklisted_peers, get_peer_by_id, get_peer_count, get_peers};
use actix_web::{web, App, HttpServer};
use avail_light_core::{
	data,
	network::p2p::{self, is_global_address, OutputEvent},
	shutdown::Controller,
	types::{PeerAddress, ProjectName},
	utils::{default_subscriber, install_panic_hooks, json_subscriber, spawn_in_span},
};
use bootstrap_monitor::BootstrapMonitor;
use clap::Parser;
use color_eyre::{eyre::Context, Result};
use config::CliOpts;
use libp2p::{Multiaddr, PeerId};
use peer_monitor::PeerMonitor;
use server::AppState;
use std::{
	collections::{HashMap, HashSet, VecDeque},
	net::{Ipv4Addr, SocketAddr},
	sync::Arc,
	time::{Duration, SystemTime},
};
use tokio::{
	select,
	sync::{mpsc::UnboundedReceiver, Mutex},
	time,
};
use tracing::{error, info, trace};
use types::ServerInfo;

mod bootstrap_monitor;
mod config;
mod peer_discovery;
mod peer_monitor;
mod server;
mod telemetry;
mod types;

// TODO: Add pruning logic that periodically goes through the list of servers and drops servers that were not seen for a while

#[tokio::main]
async fn main() -> Result<()> {
	let server_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>> =
		Arc::new(Mutex::new(HashMap::default()));

	info!("Starting Avail monitors...");
	let version = clap::crate_version!();
	let _cli = CliOpts::parse();
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
	install_panic_hooks(shutdown.clone())?;

	// Initialize telemetry
	let metrics = telemetry::MonitorMetrics::new(
		config.otel.ot_collector_endpoint.clone(),
		config.otel.ot_export_period,
		config.otel.ot_export_timeout,
	)?;
	info!(
		"Telemetry initialized with endpoint: {}",
		config.otel.ot_collector_endpoint
	);

	// Initialize metrics to 0 on startup
	metrics.set_active_peers(0);
	metrics.set_blocked_peers(0);

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

	info!("Starting event loop");

	spawn_in_span(shutdown.with_cancel(p2p_event_loop.run()));

	p2p_client
		.start_listening(config.libp2p.listeners())
		.await
		.wrap_err("Error starting listener.")?;
	info!("P2P listener started on port {}", config.libp2p.port);

	// Extract bootstrap peer IDs
	// These are tracked with the bootstrap monitor, not with server monitor
	let bootstrap_peers: HashSet<PeerId> = config
		.libp2p
		.bootstraps
		.iter()
		.filter_map(|peer_addr| match peer_addr {
			PeerAddress::PeerIdAndMultiaddr((peer_id, _)) => Some(*peer_id),
			_ => None,
		})
		.collect();

	info!("Bootstrap peers: {:?}", bootstrap_peers);

	let bootstrap_interval = time::interval(Duration::from_secs(config.bootstrap_interval));
	let peer_monitor_interval = time::interval(Duration::from_secs(config.peer_monitor_interval));
	let discovery_interval = time::interval(Duration::from_secs(config.peer_discovery_interval));

	// Start event handler to track discovered peers
	let server_list_clone = server_list.clone();
	let bootstrap_peers_clone = bootstrap_peers.clone();
	let metrics_clone = metrics.clone();
	spawn_in_span(shutdown.with_cancel(async move {
		if let Err(e) = handle_events(
			p2p_events,
			server_list_clone,
			bootstrap_peers_clone,
			metrics_clone,
		)
		.await
		{
			error!("Event handler error: {e}");
		}
	}));

	// 1. Test bootstrap availability
	let mut bootstrap_monitor = BootstrapMonitor::new(
		config.libp2p.bootstraps.clone(),
		bootstrap_interval,
		p2p_client.clone(),
		metrics.clone(),
	);
	_ = spawn_in_span(shutdown.with_cancel(async move {
		if let Err(e) = bootstrap_monitor.start_monitoring().await {
			error!("Bootstrap monitor error: {e}");
		};
	}));

	// peer discovery
	let peer_discovery = peer_discovery::PeerDiscovery::new(discovery_interval, p2p_client.clone());
	info!("Starting peer discovery");
	spawn_in_span(shutdown.with_cancel(async move {
		if let Err(e) = peer_discovery.start_discovery().await {
			error!("Peer discovery error: {e}");
		};
	}));

	// peer monitor
	let config_clone = config.clone();
	let peer_monitor = PeerMonitor::new(
		p2p_client.clone(),
		peer_monitor_interval,
		server_list.clone(),
		config_clone,
		metrics.clone(),
	);
	info!("Starting monitor part");
	_ = spawn_in_span(shutdown.with_cancel(async move {
		if let Err(e) = peer_monitor.start_monitoring().await {
			error!("Peer monitor error: {e}");
		};
	}));

	let app_state = web::Data::new(AppState {
		server_list: server_list.clone(),
		pagination: config.pagination,
	});

	let server = HttpServer::new(move || {
		App::new()
			.app_data(app_state.clone())
			.service(get_blacklisted_peers)
			.service(get_peers)
			.service(get_peer_count)
			.service(get_peer_by_id)
	})
	.bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.http_port)))?;

	match server.run().await {
		Ok(_) => {
			info!("HTTP server stopped");
		},
		Err(e) => {
			error!("HTTP server error: {}", e);
		},
	}

	Ok(())
}

pub async fn handle_events(
	mut p2p_receiver: UnboundedReceiver<OutputEvent>,
	server_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	bootstrap_peers: HashSet<PeerId>,
	metrics: telemetry::MonitorMetrics,
) -> Result<()> {
	loop {
		select! {
			Some(p2p_event) = p2p_receiver.recv() => {
				match p2p_event {
					OutputEvent::DiscoveredPeers { peers } => {
						trace!("Discovered {} peers", peers.len());

						let peers_count = peers.len() as u64;

						let mut servers = server_list.lock().await;

						for (peer_id, addresses) in peers {
							if bootstrap_peers.contains(&peer_id) {
								trace!("Skipping bootstrap peer {}", peer_id);
								continue;
							}

							let globally_reachable_addresses: Vec<Multiaddr> = addresses
								.iter()
								.filter(|addr| is_global_address(addr))
								.cloned()
								.collect();

							let is_blacklisted = globally_reachable_addresses.is_empty();

							match servers.get_mut(&peer_id) {
								Some(info) => {
									info.multiaddr = globally_reachable_addresses;
									info.last_discovered = Some(SystemTime::now());
									info.is_blacklisted = is_blacklisted;
									// We don't reset counters here because even though the addresses might be new, servers can still continue to fail (if they started failing previously)
								},
								None => {
									let server_info = ServerInfo {
										multiaddr: globally_reachable_addresses,
										last_discovered: Some(SystemTime::now()),
										last_successful_dial: None,
										last_ping_rtt: None,
										ping_records: VecDeque::with_capacity(20),
										is_blacklisted,
										connection_results: VecDeque::with_capacity(20),
									};
									servers.insert(peer_id, server_info);
								}
							}
							metrics.set_peer_blocked_status(&peer_id.to_string(), is_blacklisted);
						}

						let active_count = servers.values().filter(|s| !s.is_blacklisted).count() as u64;
						let blocked_count = servers.values().filter(|s| s.is_blacklisted).count() as u64;

						metrics.set_active_peers(active_count);
						metrics.set_blocked_peers(blocked_count);
						metrics.inc_discovered_peers(peers_count);

						info!("Total peers: {}, Active: {}, Blocked: {}", servers.len(), active_count, blocked_count);
					},
					OutputEvent::Ping { peer, rtt } => {
						let mut servers = server_list.lock().await;
						if let Some(peer_info) = servers.get_mut(&peer) {
							peer_info.update_ping_stats(rtt);
							metrics.set_peer_ping_latency(&peer.to_string(), rtt.as_millis() as f64);
						}
					},

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
