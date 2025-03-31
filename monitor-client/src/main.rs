use avail_light_core::{
	data,
	network::p2p::{self, is_multiaddr_global, OutputEvent},
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
use std::{collections::HashMap, collections::HashSet, sync::Arc, time::Duration};
use tokio::{
	select,
	sync::{mpsc::UnboundedReceiver, Mutex},
	time,
};
use tracing::{debug, error, info, trace};

mod bootstrap_monitor;
mod config;
mod peer_discovery;
mod peer_monitor;

// TODO: Add pruning logic that periodically goes through the list of servers and drops servers that were not seen for a while

#[derive(Clone, Default)]
pub struct ServerInfo {
	multiaddr: Vec<Multiaddr>,
	failed_counter: usize,
	success_counter: usize,
	// TODO: Add `last_seen`` timestamp
}

#[tokio::main]
async fn main() -> Result<()> {
	let server_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>> =
		Arc::new(Mutex::new(HashMap::default()));
	let server_black_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>> =
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

	let (p2p_keypair, _) = p2p::identity(&config.libp2p, db.clone())?;

	let (p2p_client, p2p_event_loop, p2p_events) = p2p::init(
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
	let server_black_list_clone = server_black_list.clone();
	let bootstrap_peers_clone = bootstrap_peers.clone();
	spawn_in_span(shutdown.with_cancel(async move {
		if let Err(e) = handle_events(
			p2p_events,
			server_list_clone,
			server_black_list_clone,
			bootstrap_peers_clone,
		)
		.await
		{
			error!("Event handler error: {e}");
		}
	}));

	// 1. Test bootstrap availability
	let mut bootstrap_monitor = BootstrapMonitor::new(
		config.libp2p.bootstraps,
		bootstrap_interval,
		p2p_client.clone(),
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
	let peer_monitor = PeerMonitor::new(p2p_client.clone(), peer_monitor_interval, server_list);
	info!("Starting monitor part");
	_ = spawn_in_span(shutdown.with_cancel(async move {
		if let Err(e) = peer_monitor.start_monitoring().await {
			error!("Peer monitor error: {e}");
		};
	}))
	.await;
	Ok(())
}

pub async fn handle_events(
	mut p2p_receiver: UnboundedReceiver<OutputEvent>,
	server_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	server_black_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	bootstrap_peers: HashSet<PeerId>,
) -> Result<()> {
	loop {
		select! {
			Some(p2p_event) = p2p_receiver.recv() => {
				if let OutputEvent::DiscoveredPeers { peers } = p2p_event {
							trace!("Discovered {} peers", peers.len());

							let mut servers = server_list.lock().await;
							let mut black_list = server_black_list.lock().await;
							for (peer_id, addresses) in peers {
								if bootstrap_peers.contains(&peer_id) {
									trace!("Skipping bootstrap peer {}", peer_id);
									continue;
								}

								let globally_reachable_addresses: Vec<Multiaddr> = addresses
									.iter()
									.filter(|addr| is_multiaddr_global(addr))
									.cloned()
									.collect();

								if globally_reachable_addresses.is_empty() {
									debug!("Peer {} has no globally reachable addresses, adding to blacklist", peer_id);
									black_list.entry(peer_id).or_insert_with(ServerInfo::default);
									continue;
								}

								// A new global address just appeared for the peer that previously had none
								if let Some(peer) = black_list.get(&peer_id) {
									if peer.multiaddr.is_empty() {
										trace!("Peer {} now has globally reachable addresses, removing from blacklist", peer_id);
										black_list.remove(&peer_id);
									}
								}

								match servers.get_mut(&peer_id) {
									Some(info) => {
										info.multiaddr = globally_reachable_addresses;
										// We don't reset counters here because even though the addresses might be new, servers can still continue to fail (if they started failing previously)
									},
									None => {
										let server_info = ServerInfo {
											multiaddr: globally_reachable_addresses,
											failed_counter: 0,
											success_counter: 0
										};
										servers.insert(peer_id, server_info);
									}
								}
							}
							info!("Total peers in server list: {}", servers.len());
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
