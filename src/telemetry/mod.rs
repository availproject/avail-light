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
	SessionBlock,
	OutgoingConnectionError,
	IncomingConnectionError,
	IncomingConnection,
	ConnectionEstablished,
	IncomingPutRecord,
	IncomingGetRecord,
}

impl Display for MetricCounter {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			MetricCounter::SessionBlock => write!(f, "session_block_counter"),
			MetricCounter::OutgoingConnectionError => write!(f, "outgoing_connection_errors"),
			MetricCounter::IncomingConnectionError => write!(f, "incoming_connection_errors"),
			MetricCounter::IncomingConnection => write!(f, "incoming_connections"),
			MetricCounter::ConnectionEstablished => write!(f, "established_connections"),
			MetricCounter::IncomingPutRecord => write!(f, "incoming_put_record_counter"),
			MetricCounter::IncomingGetRecord => write!(f, "incoming_get_record_counter"),
		}
	}
}

impl MetricCounter {
	fn init_counters(meter: Meter) -> HashMap<String, Counter<u64>> {
		let mut counter_map: HashMap<String, Counter<u64>> = Default::default();
		for counter in [
			MetricCounter::SessionBlock,
			MetricCounter::OutgoingConnectionError,
			MetricCounter::IncomingConnectionError,
			MetricCounter::IncomingConnection,
			MetricCounter::ConnectionEstablished,
			MetricCounter::IncomingPutRecord,
			MetricCounter::IncomingGetRecord,
		] {
			counter_map.insert(
				counter.to_string(),
				meter.u64_counter(counter.to_string()).init(),
			);
		}
		counter_map
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
	ConnectedPeersNum(usize),
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
}
