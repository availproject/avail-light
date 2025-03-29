//! LibP2P reachability monitor
//!
//! The monitor periodically dials all of the peers from the `server_list` and saves externally unreachable peers to a blacklist map
//!
//! TODO: Expose map via REST API
use color_eyre::Result;
use libp2p::{swarm::dial_opts::PeerCondition, PeerId};
use std::{collections::HashMap, sync::Arc};
use tokio::{sync::Mutex, time::Interval};
use tracing::{debug, info, trace, warn};

use avail_light_core::network::p2p::Client;

use crate::ServerInfo;

pub struct PeerMonitor {
	interval: Interval,
	p2p_client: Client,
	server_monitor_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	server_black_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
}

impl PeerMonitor {
	pub fn new(
		p2p_client: Client,
		interval: Interval,
		server_monitor_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	) -> Self {
		Self {
			interval,
			p2p_client,
			server_monitor_list,
			server_black_list: Default::default(),
		}
	}

	pub async fn start_monitoring(mut self) -> Result<()> {
		info!("Peer monitoring started");

		loop {
			self.interval.tick().await;
			info!(
				"Total peers: {}. Blacklisted peers: {}.",
				self.server_monitor_list.lock().await.len(),
				self.server_black_list.lock().await.len()
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
			let server_black_list = self.server_black_list.clone();
			let server_monitor_list = self.server_monitor_list.clone();

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
					p2p_client,
					server_black_list,
					&peer_id,
					&mut cloned_info,
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
// TODO: decide if this is the proper approach
async fn check_peer_connectivity(
	p2p_client: Client,
	server_black_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	peer_id: &PeerId,
	info: &mut ServerInfo,
) -> Result<()> {
	match p2p_client
		.dial_peer(*peer_id, info.multiaddr.clone(), PeerCondition::Always)
		.await
	{
		Ok(_) => {
			trace!("✅ Successfully dialed peer {}", peer_id);

			let mut black_list = server_black_list.lock().await;
			// If server is blacklisted and returns a successful dial:
			// 1. Increase the success counter by 1
			// 2. Reset the fail counter
			// If the success counter goes over the threshold, remove the peer from the blacklist
			if let Some(black_list_info) = black_list.get_mut(peer_id) {
				black_list_info.success_counter += 1;
				black_list_info.failed_counter = 0;

				// TODO: Parametarize
				if black_list_info.success_counter == 3 {
					black_list.remove(peer_id);
					debug!(
						"Peer {} removed from blacklist after 3 successful dials",
						peer_id
					);
				}
			}

			// Every successful dial resets the fail counter
			// TODO: Bounded counter might be a better approach here
			info.success_counter = info.success_counter.saturating_add(1);
			info.failed_counter = 0;
		},
		Err(e) => {
			debug!("❌ Failed to dial peer {}: {}", peer_id, e);
			// On every fail success counter is reset
			info.failed_counter = info.failed_counter.saturating_add(1);
			info.success_counter = 0;
			// TODO: Parametarize failed counter threshold
			if info.failed_counter == 3 {
				debug!(
					"⚠️ Peer {} has been unreachable for 3 consecutive attempts!",
					peer_id
				);
				server_black_list
					.lock()
					.await
					.insert(*peer_id, info.clone());
			}
		},
	}

	Ok(())
}
