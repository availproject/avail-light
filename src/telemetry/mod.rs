use std::{
	collections::HashMap,
	fmt::{self, Display, Formatter},
};

use async_trait::async_trait;
use color_eyre::Result;
use mockall::automock;
use opentelemetry_api::metrics::{Counter, Meter};

pub mod otlp;

pub enum MetricCounter {
	Starts,
}

impl Display for MetricCounter {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			MetricCounter::Starts => write!(f, "avail.light.starts"),
		}
	}
}

impl MetricCounter {
	fn init_counters(meter: Meter) -> HashMap<String, Counter<u64>> {
		[MetricCounter::Starts]
			.iter()
			.map(|counter| {
				(
					counter.to_string(),
					meter.u64_counter(counter.to_string()).init(),
				)
			})
			.collect()
	}
}

pub enum MetricValue {
	TotalBlockNumber(u32),
	DHTFetched(f64),
	DHTFetchedPercentage(f64),
	DHTFetchDuration(f64),
	NodeRPCFetched(f64),
	NodeRPCFetchDuration(f64),
	BlockConfidence(f64),
	BlockConfidenceTreshold(f64),
	RPCCallDuration(f64),
	DHTPutDuration(f64),
	DHTPutSuccess(f64),
	KadRoutingPeerNum(usize),
	HealthCheck(),
	BlockProcessingDelay(f64),
	PingLatency(f64),
	ReplicationFactor(u16),
	QueryTimeout(u32),
	#[cfg(feature = "crawl")]
	CrawlCellsSuccessRate(f64),
	#[cfg(feature = "crawl")]
	CrawlRowsSuccessRate(f64),
	#[cfg(feature = "crawl")]
	CrawlBlockDelay(f64),
}

#[automock]
#[async_trait]
pub trait Metrics {
	async fn count(&self, counter: MetricCounter);
	async fn record(&self, value: MetricValue) -> Result<()>;
	async fn set_multiaddress(&self, multiaddr: String);
	async fn set_ip(&self, ip: String);
	async fn get_multiaddress(&self) -> String;
}
