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

use crate::{config::Config, ServerInfo};
pub struct PeerMonitor {
	p2p_client: Client,
	interval: Interval,
	server_monitor_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	config: Config,
}
impl PeerMonitor {
	pub fn new(
		p2p_client: Client,
		interval: Interval,
		server_monitor_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
		config: Config,
	) -> Self {
		Self {
			interval,
			p2p_client,
			server_monitor_list,
			config,
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
				_ = check_peer_connectivity(&config, p2p_client, &peer_id, &mut cloned_info).await;

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
// TODO: decide if this is the proper approach
async fn check_peer_connectivity(
	config: &Config,
	p2p_client: Client,
	peer_id: &PeerId,
	info: &mut ServerInfo,
) -> Result<()> {
	// NOTE: `PeerCondition::NotDialing` might not be the best suitable dial condition for our approach
	match p2p_client
		.dial_peer(*peer_id, info.multiaddr.clone(), PeerCondition::NotDialing)
		.await
	{
		Ok(_) => {
			trace!("✅ Successfully dialed peer {}", peer_id);

			// If server is blacklisted and returns a successful dial:
			// 1. Increase the success counter by 1
			// 2. Reset the fail counter
			// If the success counter goes over the threshold, remove the peer from the blacklist
			if info.is_blacklisted {
				info.success_counter += 1;

				if info.success_counter >= config.success_threshold {
					info.is_blacklisted = false;
					info.success_counter = 0; // Reset after unmarking
					debug!(
						"Peer {} unmarked as blacklisted after {} successful dials",
						peer_id, config.success_threshold
					);
				}
			} else {
				// For non-blacklisted peers, just increment the success counter and reset the failure counter
				info.success_counter = info.success_counter.saturating_add(1);
			}

			// Every successful dial resets the fail counter for all peers
			info.failed_counter = 0;
			info.last_successful_dial = Some(SystemTime::now())
		},
		Err(e) => {
			debug!("❌ Failed to dial peer {}: {}", peer_id, e);
			// On every fail success counter is reset
			info.failed_counter = info.failed_counter.saturating_add(1);
			info.success_counter = 0;
			if info.failed_counter >= config.fail_threshold {
				debug!(
                    "⚠️ Peer {} has been unreachable for {} consecutive attempts, marking as blacklisted!",
                    peer_id, config.fail_threshold
                );
				info.is_blacklisted = true;
			}
		},
	}

	Ok(())
}
