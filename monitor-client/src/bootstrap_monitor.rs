use avail_light_core::{network::p2p::Client, types::PeerAddress};
use color_eyre::Result;
use tokio::time::Interval;
use tracing::{error, info};

pub struct BootstrapMonitor {
	bootstraps: Vec<PeerAddress>,
	interval: Interval,
	p2p_client: Client,
}

impl BootstrapMonitor {
	pub fn new(bootstraps: Vec<PeerAddress>, interval: Interval, p2p_client: Client) -> Self {
		Self {
			bootstraps,
			interval,
			p2p_client,
		}
	}

	pub async fn start_monitoring(&mut self) -> Result<()> {
		info!("Bootstrap monitor started.");
		loop {
			self.interval.tick().await;

			for (peer, addr) in self.bootstraps.iter().map(Into::into) {
				match self.p2p_client.dial_peer(peer, vec![addr.clone()]).await {
					Ok(_) => {
						info!("Bootstrap {peer} dialed successfully!");
					},
					Err(e) => {
						error!("Error dialing bootstrap: {e}");
					},
				}
			}
		}
	}
}
