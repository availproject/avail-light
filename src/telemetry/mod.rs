use anyhow::Result;
use async_trait::async_trait;
use mockall::automock;

pub mod otlp;

pub enum MetricCounter {
	SessionBlock,
}

pub enum MetricValue {
	TotalBlockNumber(u32),
	DHTFetched(f64),
	DHTFetchedPercentage(f64),
	NodeRPCFetched(f64),
	BlockConfidence(f64),
	RPCCallDuration(f64),
	DHTPutDuration(f64),
	DHTPutSuccess(f64),
	DHTPutRowsDuration(f64),
	DHTPutRowsSuccess(f64),
	KadRoutingPeerNum(usize),
	HealthCheck(),
	BlockProcessingDelay(f64),
	#[cfg(feature = "crawl")]
	CrawlCellsSuccessRate(f64),
	#[cfg(feature = "crawl")]
	CrawlRowsSuccessRate(f64),
}

#[automock]
#[async_trait]
pub trait Metrics {
	async fn count(&self, counter: MetricCounter);
	async fn record(&self, value: MetricValue) -> Result<()>;
	async fn set_multiaddress(&self, multiaddr: String);
	async fn set_ip(&self, ip: String);
}
