use libp2p::{Multiaddr, PeerId};
use statrs::statistics::Statistics;

use std::{
	collections::VecDeque,
	time::{Duration, SystemTime},
};

use tracing::{debug, info};

const MAX_PING_RECORDS: usize = 20;

#[derive(Clone, Default)]
pub struct ServerInfo {
	pub multiaddr: Vec<Multiaddr>,
	pub failed_counter: usize,
	pub success_counter: usize,
	pub last_successful_dial: Option<SystemTime>,
	pub last_discovered: Option<SystemTime>,
	pub last_ping_rtt: Option<Duration>,
	pub ping_records: VecDeque<f64>,
	pub is_blacklisted: bool,
}

impl ServerInfo {
	pub fn update_ping_stats(&mut self, rtt: Duration) {
		self.last_ping_rtt = Some(rtt);

		self.ping_records.push_back(rtt.as_millis() as f64);
		// Maintain a kernel of MAX_PING_RECORDS
		if self.ping_records.len() > MAX_PING_RECORDS {
			self.ping_records.pop_front();
		}
	}

	// Helpers for future statistical analysis
	#[allow(dead_code)]
	pub fn min_ping(&self) -> Option<Duration> {
		if self.ping_records.is_empty() {
			return None;
		}

		let data: Vec<f64> = self.ping_records.iter().copied().collect();
		let min_ms = data.min();

		Some(Duration::from_secs_f64(min_ms / 1000.0))
	}

	#[allow(dead_code)]
	pub fn max_ping(&self) -> Option<Duration> {
		if self.ping_records.is_empty() {
			return None;
		}

		let data: Vec<f64> = self.ping_records.iter().copied().collect();
		let max_ms = data.max();

		Some(Duration::from_secs_f64(max_ms / 1000.0))
	}

	pub fn avg_ping(&self) -> Option<Duration> {
		if self.ping_records.is_empty() {
			return None;
		}

		let data: Vec<f64> = self.ping_records.iter().copied().collect();
		let mean_ms = data.mean();

		Some(Duration::from_secs_f64(mean_ms / 1000.0))
	}

	#[allow(dead_code)]
	pub fn log_ping_stats(&self, peer_id: &PeerId) {
		if self.ping_records.is_empty() {
			debug!("Peer {}: No ping statistics available", peer_id);
			return;
		}

		info!(
			"Peer {} ping stats (samples={}): min={:?}, max={:?}, avg={:?}",
			peer_id,
			self.ping_records.len(),
			self.min_ping(),
			self.max_ping(),
			self.avg_ping()
		);
	}
}
