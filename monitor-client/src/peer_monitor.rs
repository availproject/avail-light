//! LibP2P reachability monitor
//!
//! The monitor periodically dials all of the peers from the `server_list` and saves
//!
//! - externally unreachable peers
//! - peers without a public address (which are also externally unreachable)
use color_eyre::Result;
use libp2p::{swarm::dial_opts::PeerCondition, PeerId};
use std::{collections::HashMap, sync::Arc};
use tokio::{sync::Mutex, time::Interval};
use tracing::{debug, info, warn};

use avail_light_core::network::p2p::Client;

use crate::ServerInfo;

pub struct PeerMonitor {
	interval: Interval,
	p2p_client: Client,
	server_monitor_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	server_black_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
}

impl PeerMonitor {
	pub fn new(interval: Interval, p2p_client: Client) -> Self {
		Self {
			interval,
			p2p_client,
			server_monitor_list: Default::default(),
			server_black_list: Default::default(),
		}
	}

	pub async fn start_monitoring(mut self) -> Result<()> {
		info!("Peer monitoring started");

		loop {
			self.interval.tick().await;
			if let Err(e) = self.process_peers().await {
				warn!("Error processing peers: {}", e);
			}
		}
	}

	async fn process_peers(&mut self) -> Result<()> {
		let mut server_list = self.server_monitor_list.lock().await;
		for (peer_id, info) in server_list.iter_mut() {
			tokio::spawn(check_peer_connectivity(
				self.p2p_client.clone(),
				self.server_black_list.clone(),
				peer_id,
				info,
			));
		}

		Ok(())
	}
}

async fn check_peer_connectivity(
	p2p_client: Client,
	server_black_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	peer_id: &PeerId,
	info: &mut ServerInfo,
) -> Result<()> {
	match p2p_client
		.dial_peer(*peer_id, info.multiaddr.clone(), PeerCondition::NotDialing)
		.await
	{
		Ok(_) => {
			debug!("✅ Successfully dialed peer {}", peer_id);
			info.success_counter += 1;
			info.failed_counter = 0;
			// TODO: If blacklisted and started being reachable, remove from the blacklist after a threshold of succesful dials
		},
		Err(e) => {
			debug!("❌ Failed to dial peer {}: {}", peer_id, e);
			info.failed_counter += 1;
			// TODO: Parametarize failed counter threshold
			if info.failed_counter >= 3 {
				warn!(
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
