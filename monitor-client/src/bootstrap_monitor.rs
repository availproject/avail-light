use crate::telemetry::MonitorMetrics;
use avail_light_core::{network::p2p::Client, types::PeerAddress};
use color_eyre::Result;
use libp2p::swarm::dial_opts::PeerCondition;
use tokio::time::Interval;
use tracing::{error, info};

pub struct BootstrapMonitor {
	bootstraps: Vec<PeerAddress>,
	interval: Interval,
	p2p_client: Client,
	metrics: MonitorMetrics,
}

impl BootstrapMonitor {
	pub fn new(
		bootstraps: Vec<PeerAddress>,
		interval: Interval,
		p2p_client: Client,
		metrics: MonitorMetrics,
	) -> Self {
		Self {
			bootstraps,
			interval,
			p2p_client,
			metrics,
		}
	}

	pub async fn start_monitoring(&mut self) -> Result<()> {
		info!("Bootstrap monitor started.");
		loop {
			self.interval.tick().await;

			for (peer, addr) in self.bootstraps.iter().map(Into::into) {
				self.metrics.inc_bootstrap_attempts();
				match self
					.p2p_client
					.dial_peer(peer, vec![addr.clone()], PeerCondition::Always)
					.await
				{
					Ok(_) => {
						info!("Bootstrap {peer} dialed successfully!");
					},
					Err(e) => {
						error!("Error dialing bootstrap: {e}");
						self.metrics.inc_bootstrap_failures();
					},
				}
			}
		}
	}
}
