use avail_light_core::network::p2p::Client;
use color_eyre::Result;
use libp2p::PeerId;
use tokio::time::Interval;
use tracing::{trace, warn};

pub struct PeerDiscovery {
	interval: Interval,
	p2p_client: Client,
}

impl PeerDiscovery {
	pub fn new(interval: Interval, p2p_client: Client) -> Self {
		Self {
			interval,
			p2p_client,
		}
	}

	// Periodically dispatches closest peers query for a randomly generated peer id
	// Generated peer ids (should) follow a uniform distribution
	pub async fn start_discovery(mut self) -> Result<()> {
		loop {
			self.interval.tick().await;

			let random_peer_id = PeerId::random();
			trace!("Firing for peer: {}", random_peer_id);
			match self.p2p_client.get_closest_peers(random_peer_id).await {
				Ok(_) => {},
				Err(e) => {
					warn!("Failed to initiate query: {}", e);
				},
			}
		}
	}
}
