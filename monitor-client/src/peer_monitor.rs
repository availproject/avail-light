//! LibP2P reachability monitor
//!
//! The monitor periodically dials all of the peers from the `server_list` and saves externally unreachable peers to a blacklist map
//!
//! TODO: Expose map via REST API
use color_eyre::Result;
use libp2p::{swarm::dial_opts::PeerCondition, PeerId};
use std::{collections::HashMap, sync::Arc, time::SystemTime};
use tokio::{sync::Mutex, time::Interval};
use tracing::{debug, info, trace, warn};

use avail_light_core::network::p2p::Client;

use crate::{config::Config, telemetry::MonitorMetrics, ServerInfo};
pub struct PeerMonitor {
	p2p_client: Client,
	interval: Interval,
	server_monitor_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	config: Config,
	metrics: MonitorMetrics,
}
impl PeerMonitor {
	pub fn new(
		p2p_client: Client,
		interval: Interval,
		server_monitor_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
		config: Config,
		metrics: MonitorMetrics,
	) -> Self {
		Self {
			interval,
			p2p_client,
			server_monitor_list,
			config,
			metrics,
		}
	}

	pub async fn start_monitoring(mut self) -> Result<()> {
		info!("Peer monitoring started");

		loop {
			self.interval.tick().await;

			let blacklisted_count = self
				.server_monitor_list
				.lock()
				.await
				.values()
				.filter(|info| info.is_blacklisted)
				.count();

			info!(
				"Total peers: {}. Blacklisted peers: {}.",
				self.server_monitor_list.lock().await.len(),
				blacklisted_count
			);
			if let Err(e) = self.process_peers().await {
				warn!("Error processing peers: {}", e);
			}
		}
	}

	async fn process_peers(&mut self) -> Result<()> {
		let peer_ids: Vec<PeerId> = {
			let server_list = self.server_monitor_list.lock().await;
			server_list.keys().cloned().collect()
		};

		for peer_id in peer_ids {
			let p2p_client = self.p2p_client.clone();
			let server_monitor_list = self.server_monitor_list.clone();
			let config = self.config.clone();
			let metrics = self.metrics.clone();

			tokio::spawn(async move {
				let mut cloned_info = {
					let server_list = server_monitor_list.lock().await;
					if let Some(info) = server_list.get(&peer_id) {
						info.clone()
					} else {
						debug!("Peer {} no longer in server list", peer_id);
						return;
					}
				};

				// TODO: Handle the result
				_ = check_peer_connectivity(
					&config,
					p2p_client,
					&peer_id,
					&mut cloned_info,
					&metrics,
				)
				.await;

				let mut server_list = server_monitor_list.lock().await;
				if let Some(info) = server_list.get_mut(&peer_id) {
					*info = cloned_info;
				}
			});
		}

		Ok(())
	}
}

// Blacklisted peers remain in the server monitor list
// Peers are blacklisted if health score < 20% for the configured duration
// Peers are unblacklisted if health score > 60% for the configured duration
async fn check_peer_connectivity(
	config: &Config,
	p2p_client: Client,
	peer_id: &PeerId,
	info: &mut ServerInfo,
	metrics: &MonitorMetrics,
) -> Result<()> {
	// NOTE: `PeerCondition::NotDialing` might not be the best suitable dial condition for our approach
	match p2p_client
		.dial_peer(*peer_id, info.multiaddr.clone(), PeerCondition::NotDialing)
		.await
	{
		Ok(_) => {
			trace!("✅ Successfully dialed peer {}", peer_id);

			// Track this successful connection
			// TODO: Use ConnectionEstablishedInfo instead of bool1
			info.update_connection_result(true);
			info.last_successful_dial = Some(SystemTime::now())
		},
		Err(e) => {
			debug!("❌ Failed to dial peer {}: {}", peer_id, e);

			// Track this failed connection
			info.update_connection_result(false);
		},
	}

	// Calculate current health score
	let health_score = calculate_health_score(info);
	let now = SystemTime::now();
	let blacklist_duration =
		std::time::Duration::from_secs(config.blacklist_duration_hours * 60 * 60);

	// Check blacklisting conditions
	if !info.is_blacklisted {
		// Check if health score dropped below 20%
		if health_score < 20.0 {
			match info.below_threshold_since {
				Some(timestamp) => {
					// Check if it's been below 20% for the configured duration
					if now.duration_since(timestamp).unwrap_or_default() >= blacklist_duration {
						info.is_blacklisted = true;
						info.below_threshold_since = None;
						debug!(
							"⚠️ Peer {} blacklisted after {} hours with health score below 20%",
							peer_id, config.blacklist_duration_hours
						);
						metrics.set_peer_blocked_status(&peer_id.to_string(), true);
					}
				},
				None => {
					// First time dropping below 20%
					info.below_threshold_since = Some(now);
					debug!(
						"Peer {} health score dropped below 20% ({}%)",
						peer_id, health_score
					);
				},
			}
		} else {
			info.below_threshold_since = None;
		}
	} else {
		// Peer is blacklisted, check if health score is above 60%
		if health_score > 60.0 {
			match info.above_threshold_since {
				Some(timestamp) => {
					// Check if it's been above 60% for the configured duration
					if now.duration_since(timestamp).unwrap_or_default() >= blacklist_duration {
						info.is_blacklisted = false;
						info.above_threshold_since = None; // Reset the timer
						debug!(
							"✅ Peer {} unblacklisted after {} hours with health score above 60%",
							peer_id, config.blacklist_duration_hours
						);
						metrics.set_peer_blocked_status(&peer_id.to_string(), false);
					}
				},
				None => {
					// First time rising above 60%
					info.above_threshold_since = Some(now);
					debug!(
						"Peer {} health score rose above 60% ({}%)",
						peer_id, health_score
					);
				},
			}
		} else {
			// Health score is below 60%, reset the timer
			info.above_threshold_since = None;
		}
	}

	// Update health score metric
	metrics.set_peer_health_score(&peer_id.to_string(), health_score);

	Ok(())
}

fn calculate_health_score(info: &ServerInfo) -> f64 {
	// Health score calculation:
	// Returns 50% until we have 20 total connections
	// After 20 connections: successful_dials / total_dials * 100
	// Returns a value between 0.0 and 100.0

	let total_connections = info.connection_results.len();

	if total_connections < 20 {
		50.0
	} else {
		let successful_connections = info
			.connection_results
			.iter()
			.filter(|&&result| result)
			.count();
		(successful_connections as f64 / total_connections as f64) * 100.0
	}
}
