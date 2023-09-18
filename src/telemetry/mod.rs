use anyhow::Result;
use mockall::automock;

pub mod otlp;

pub enum MetricCounter {
	SessionBlock,
}

pub struct NetworkDumpEvent {
	pub routing_table_num_of_peers: usize,
	pub current_multiaddress: String,
	pub current_ip: String,
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
	KadRoutingTablePeerNum(u32),
}

#[automock]
pub trait Metrics {
	fn count(&self, counter: MetricCounter);
	fn record(&self, value: MetricValue) -> Result<()>;
}
