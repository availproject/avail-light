use crate::types::Origin;
use async_trait::async_trait;
use color_eyre::Result;
use mockall::automock;

pub mod otlp;

#[derive(Debug, PartialEq)]
pub enum MetricCounter {
	Starts,
	Up,
	SessionBlocks,
	OutgoingConnectionErrors,
	IncomingConnectionErrors,
	IncomingConnections,
	EstablishedConnections,
	IncomingPutRecord,
	IncomingGetRecord,
}

pub trait MetricName {
	fn name(&self) -> &'static str;
}

impl MetricName for MetricCounter {
	fn name(&self) -> &'static str {
		use MetricCounter::*;
		match self {
			Starts => "avail.light.starts",
			Up => "avail.light.up",
			SessionBlocks => "avail.light.session_blocks",
			OutgoingConnectionErrors => "avail.light.outgoing_connection_errors",
			IncomingConnectionErrors => "avail.light.incoming_connection_errors",
			IncomingConnections => "avail.light.incoming_connections",
			EstablishedConnections => "avail.light.established_connections",
			IncomingPutRecord => "avail.light.incoming_put_record",
			IncomingGetRecord => "avail.light.incoming_get_record",
		}
	}
}

impl MetricCounter {
	fn is_buffered(&self) -> bool {
		!matches!(self, MetricCounter::Starts)
	}

	fn as_last(&self) -> bool {
		matches!(self, MetricCounter::Up)
	}

	fn is_allowed(&self, origin: &Origin) -> bool {
		match (origin, self) {
			(Origin::External, MetricCounter::Starts) => true,
			(Origin::External, MetricCounter::Up) => true,
			(Origin::External, _) => false,
			(_, _) => true,
		}
	}
}

#[derive(Clone, Debug)]
pub enum MetricValue {
	BlockHeight(u32),
	BlockConfidence(f64),
	BlockConfidenceThreshold(f64),
	BlockProcessingDelay(f64),

	DHTReplicationFactor(u16),

	DHTFetched(f64),
	DHTFetchedPercentage(f64),
	DHTFetchDuration(f64),
	DHTPutDuration(f64),
	DHTPutSuccess(f64),

	DHTConnectedPeers(usize),
	DHTQueryTimeout(u32),
	DHTPingLatency(f64),

	RPCFetched(f64),
	RPCFetchDuration(f64),
	RPCCallDuration(f64),

	#[cfg(feature = "crawl")]
	CrawlCellsSuccessRate(f64),
	#[cfg(feature = "crawl")]
	CrawlRowsSuccessRate(f64),
	#[cfg(feature = "crawl")]
	CrawlBlockDelay(f64),
}

impl MetricName for MetricValue {
	fn name(&self) -> &'static str {
		use MetricValue::*;

		match self {
			BlockHeight(_) => "avail.light.block.height",
			BlockConfidence(_) => "avail.light.block.confidence",
			BlockConfidenceThreshold(_) => "avail.light.block.confidence_threshold",
			BlockProcessingDelay(_) => "avail.light.block.processing_delay",

			DHTReplicationFactor(_) => "avail.light.dht.replication_factor",
			DHTFetched(_) => "avail.light.dht.fetched",
			DHTFetchedPercentage(_) => "avail.light.dht.fetched_percentage",
			DHTFetchDuration(_) => "avail.light.dht.fetch_duration",
			DHTPutDuration(_) => "avail.light.dht.put_duration",
			DHTPutSuccess(_) => "avail.light.dht.put_success",

			DHTConnectedPeers(_) => "avail.light.dht.connected_peers",
			DHTQueryTimeout(_) => "avail.light.dht.query_timeout",
			DHTPingLatency(_) => "avail.light.dht.ping_latency",

			RPCFetched(_) => "avail.light.rpc.fetched",
			RPCFetchDuration(_) => "avail.light.rpc.fetch_duration",
			RPCCallDuration(_) => "avail.light.rpc.call_duration",

			#[cfg(feature = "crawl")]
			CrawlCellsSuccessRate(_) => "avail.light.crawl.cells_success_rate",
			#[cfg(feature = "crawl")]
			CrawlRowsSuccessRate(_) => "avail.light.crawl.rows_success_rate",
			#[cfg(feature = "crawl")]
			CrawlBlockDelay(_) => "avail.light.crawl.block_delay",
		}
	}
}

impl MetricValue {
	// Metric filter for external peers
	// Only the metrics we wish to send to OTel should be in this list
	fn is_allowed(&self, origin: &Origin) -> bool {
		match origin {
			Origin::External => matches!(
				self,
				MetricValue::DHTFetchedPercentage(_) | MetricValue::BlockConfidence(_)
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
