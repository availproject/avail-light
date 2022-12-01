use std::sync::atomic::AtomicU64;

use prometheus_client::{
	metrics::{counter::Counter, gauge::Gauge},
	registry::Registry,
};

// Defines metrics struct that is used with Prometheus for light client.
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
}

pub enum MetricEvent {
	SessionBlockCounter,
	TotalBlockNumber(u32),
	DHTFetched(u64),
	DHTFetchedPercentage(f64),
	NodeRPCFetched(u64),
	BlockConfidence(f64),
	RPCCallDuration(f64),
	DHTPutDuration(f64),
	DHTPutSuccess(f64),
}

impl Metrics {
	pub fn new(registry: &mut Registry) -> Self {
		let sub_reg = registry.sub_registry_with_prefix("lc_metrics");

		let session_block_counter = Counter::default();
		sub_reg.register(
			"session_block_number",
			"Number of blocks processed by the light client since (re)start",
			Box::new(session_block_counter.clone()),
		);

		let total_block_number = Gauge::default();
		sub_reg.register(
			"total_block_number",
			"Current block number (as received from Avail header)",
			Box::new(total_block_number.clone()),
		);

		let dht_fetched = Gauge::default();
		sub_reg.register(
			"dht_fetched",
			"Number of cells fetched from DHT",
			Box::new(dht_fetched.clone()),
		);

		let dht_fetched_percentage = Gauge::default();
		sub_reg.register(
			"dht_fetched_percentage",
			"Percentage of cells fetched via DHT compared to total number of cells requested for random sampling",
			Box::new(dht_fetched_percentage.clone()),
		);

		let node_rpc_fetched = Gauge::default();
		sub_reg.register(
			"node_rpc_fetched",
			"Number of cells fetched via RPC call to node",
			Box::new(node_rpc_fetched.clone()),
		);

		let block_confidence = Gauge::<f64, AtomicU64>::default();
		sub_reg.register(
			"block_confidence",
			"Block confidence of current block",
			Box::new(block_confidence.clone()),
		);

		let rpc_call_duration = Gauge::default();
		sub_reg.register(
			"rpc_call_duration",
			"Time needed to retrieve all cells via RPC for current block (in seconds)",
			Box::new(rpc_call_duration.clone()),
		);

		let dht_put_duration = Gauge::default();
		sub_reg.register(
			"dht_put_duration",
			"Time needed to perform DHT PUT operation for current block (in seconds)",
			Box::new(dht_put_duration.clone()),
		);

		let dht_put_success = Gauge::default();
		sub_reg.register(
			"dht_put_sucess_rate",
			"Success rate of the DHT PUT operation",
			Box::new(dht_put_success.clone()),
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
		}
	}
}
