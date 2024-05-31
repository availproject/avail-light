use std::{
	collections::HashMap,
	fmt::{self, Display, Formatter},
};

use async_trait::async_trait;
use color_eyre::Result;
use mockall::automock;
use opentelemetry_api::metrics::{Counter, Meter};

use crate::types::Origin;

pub mod otlp;

#[derive(Debug)]
pub enum MetricCounter {
	Starts,
	SessionBlockCounter,
	OutgoingConnectionErrors,
	IncomingConnectionErrors,
	IncomingConnections,
	EstablishedConnections,
	IncomingPutRecordCounter,
	IncomingGetRecordCounter,
}

impl Display for MetricCounter {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			MetricCounter::Starts => write!(f, "avail.light.starts"),
			MetricCounter::SessionBlockCounter => write!(f, "session_block_counter"),
			MetricCounter::OutgoingConnectionErrors => write!(f, "outgoing_connection_errors"),
			MetricCounter::IncomingConnectionErrors => write!(f, "incoming_connection_errors"),
			MetricCounter::IncomingConnections => write!(f, "incoming_connections"),
			MetricCounter::EstablishedConnections => write!(f, "established_connections"),
			MetricCounter::IncomingPutRecordCounter => write!(f, "incoming_put_record_counter"),
			MetricCounter::IncomingGetRecordCounter => write!(f, "incoming_get_record_counter"),
		}
	}
}

impl MetricCounter {
	fn is_buffered(&self) -> bool {
		!matches!(self, MetricCounter::Starts)
	}

	fn is_allowed(&self, origin: &Origin) -> bool {
		match (origin, self) {
			(Origin::External, MetricCounter::Starts) => true,
			(Origin::External, _) => false,
			(_, _) => true,
		}
	}

	fn init_counters(meter: Meter, origin: Origin) -> HashMap<String, Counter<u64>> {
		[
			MetricCounter::Starts,
			MetricCounter::SessionBlockCounter,
			MetricCounter::OutgoingConnectionErrors,
			MetricCounter::IncomingConnectionErrors,
			MetricCounter::IncomingConnections,
			MetricCounter::EstablishedConnections,
			MetricCounter::IncomingPutRecordCounter,
			MetricCounter::IncomingGetRecordCounter,
		]
		.iter()
		.filter(|counter| MetricCounter::is_allowed(counter, &origin))
		.map(|counter| {
			(
				counter.to_string(),
				meter.u64_counter(counter.to_string()).init(),
			)
		})
		.collect()
	}
}

#[derive(Clone, Debug)]
pub enum MetricValue {
	TotalBlockNumber(u32),
	DHTFetched(f64),
	DHTFetchedPercentage(f64),
	DHTFetchDuration(f64),
	NodeRPCFetched(f64),
	NodeRPCFetchDuration(f64),
	BlockConfidence(f64),
	BlockConfidenceThreshold(f64),
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

impl MetricValue {
	// Metric filter for external peers
	// Only the metrics we wish to send to OTel should be in this list
	fn is_allowed(&self, origin: &Origin) -> bool {
		match origin {
			Origin::External => matches!(
				self,
				MetricValue::DHTFetchedPercentage(_)
					| MetricValue::BlockConfidence(_)
					| MetricValue::HealthCheck()
			),
			_ => true,
		}
	}
}

#[automock]
#[async_trait]
pub trait Metrics {
	async fn count(&self, counter: MetricCounter);
	async fn record(&self, value: MetricValue);
	async fn flush(&self) -> Result<()>;
}
