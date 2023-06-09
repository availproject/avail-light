use std::sync::atomic::AtomicU64;

use prometheus_client::{
	metrics::{counter::Counter, gauge::Gauge},
	registry::Registry,
};

// Defines metrics struct that is used with Prometheus for light client.
#[derive(Clone)]
pub struct Metrics {
	session_block_counter: Counter,
	total_block_number: Gauge,
	dht_fetched: Gauge,
	dht_fetched_percentage: Gauge<f64, AtomicU64>,
	node_rpc_fetched: Gauge,
	block_confidence: Gauge<f64, AtomicU64>,
	rpc_call_duration: Gauge<f64, AtomicU64>,
	dht_put_duration: Gauge<f64, AtomicU64>,
	dht_put_success: Gauge<f64, AtomicU64>,
	dht_put_rows_duration: Gauge<f64, AtomicU64>,
	dht_put_rows_success: Gauge<f64, AtomicU64>,
	kad_routing_table_peer_num: Gauge,
}

pub enum MetricEvent {
	SessionBlockCounter,
	TotalBlockNumber(u32),
	DHTFetched(i64),
	DHTFetchedPercentage(f64),
	NodeRPCFetched(i64),
	BlockConfidence(f64),
	RPCCallDuration(f64),
	DHTPutDuration(f64),
	DHTPutSuccess(f64),
	DHTPutRowsDuration(f64),
	DHTPutRowsSuccess(f64),
	KadRoutingTablePeerNum(u32),
}

impl Metrics {
	pub fn new(registry: &mut Registry) -> Self {
		let sub_reg = registry.sub_registry_with_prefix("lc_metrics");

		let session_block_counter = Counter::default();
		sub_reg.register(
			"session_block_number",
			"Number of blocks processed by the light client since (re)start",
			session_block_counter.clone(),
		);

		let total_block_number = Gauge::default();
		sub_reg.register(
			"total_block_number",
			"Current block number (as received from Avail header)",
			total_block_number.clone(),
		);

		let dht_fetched = Gauge::default();
		sub_reg.register(
			"dht_fetched",
			"Number of cells fetched from DHT",
			dht_fetched.clone(),
		);

		let dht_fetched_percentage = Gauge::default();
		sub_reg.register(
			"dht_fetched_percentage",
			"Percentage of cells fetched via DHT compared to total number of cells requested for random sampling",
			dht_fetched_percentage.clone(),
		);

		let node_rpc_fetched = Gauge::default();
		sub_reg.register(
			"node_rpc_fetched",
			"Number of cells fetched via RPC call to node",
			node_rpc_fetched.clone(),
		);

		let block_confidence = Gauge::<f64, AtomicU64>::default();
		sub_reg.register(
			"block_confidence",
			"Block confidence of current block",
			block_confidence.clone(),
		);

		let rpc_call_duration = Gauge::default();
		sub_reg.register(
			"rpc_call_duration",
			"Time needed to retrieve all cells via RPC for current block (in seconds)",
			rpc_call_duration.clone(),
		);

		let dht_put_duration = Gauge::default();
		sub_reg.register(
			"dht_put_duration",
			"Time needed to perform DHT PUT operation for current block (in seconds)",
			dht_put_duration.clone(),
		);

		let dht_put_success = Gauge::default();
		sub_reg.register(
			"dht_put_sucess_rate",
			"Success rate of the DHT PUT operation",
			dht_put_success.clone(),
		);

		let dht_put_rows_duration = Gauge::default();
		sub_reg.register(
			"dht_put_rows_duration",
			"Time needed to perform DHT PUT rows operation for current block (in seconds)",
			dht_put_rows_duration.clone(),
		);

		let dht_put_rows_success = Gauge::default();
		sub_reg.register(
			"dht_put_rows_sucess_rate",
			"Success rate of the DHT PUT rows operation",
			dht_put_success.clone(),
		);
		let kad_routing_table_peer_num = Gauge::default();
		sub_reg.register(
			"kad_routing_table_peer_num",
			"Number of connected peers in clients routing table",
			kad_routing_table_peer_num.clone(),
		);

		Self {
			session_block_counter,
			total_block_number,
			dht_fetched,
			dht_fetched_percentage,
			node_rpc_fetched,
			block_confidence,
			rpc_call_duration,
			dht_put_duration,
			dht_put_success,
			dht_put_rows_duration,
			dht_put_rows_success,
			kad_routing_table_peer_num,
		}
	}

	pub fn record(&self, event: MetricEvent) {
		match event {
			MetricEvent::SessionBlockCounter => {
				self.session_block_counter.inc();
			},
			MetricEvent::TotalBlockNumber(num) => {
				self.total_block_number.set(num.into());
			},
			MetricEvent::DHTFetched(num) => {
				self.dht_fetched.set(num);
			},
			MetricEvent::DHTFetchedPercentage(num) => {
				self.dht_fetched_percentage.set(num);
			},
			MetricEvent::NodeRPCFetched(num) => {
				self.node_rpc_fetched.set(num);
			},
			MetricEvent::BlockConfidence(num) => {
				self.block_confidence.set(num);
			},
			MetricEvent::RPCCallDuration(num) => {
				self.rpc_call_duration.set(num);
			},
			MetricEvent::DHTPutDuration(num) => {
				self.dht_put_duration.set(num);
			},
			MetricEvent::DHTPutSuccess(num) => {
				self.dht_put_success.set(num);
			},
			MetricEvent::DHTPutRowsDuration(num) => {
				self.dht_put_rows_duration.set(num);
			},
			MetricEvent::DHTPutRowsSuccess(num) => {
				self.dht_put_rows_success.set(num);
			},
			MetricEvent::KadRoutingTablePeerNum(num) => {
				self.kad_routing_table_peer_num.set(num.into());
			},
		}
	}
}
