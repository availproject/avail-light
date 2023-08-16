use std::time::Duration;

use anyhow::{Error, Ok, Result};
use opentelemetry_api::{
	global,
	metrics::{Counter, Meter},
	KeyValue,
};
use opentelemetry_otlp::{ExportConfig, Protocol, WithExportConfig};

#[derive(Debug, Clone)]
pub struct OTMetrics {
	pub global_meter: Meter,
	pub session_block_counter: Counter<u64>,
}

pub enum OTMetricEvent {
	SessionBlockCounter(Counter<u64>),
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

pub fn initialize_open_telemetry(endpoint: String) -> Result<OTMetrics, Error> {
	let export_config = ExportConfig {
		endpoint,
		timeout: Duration::from_secs(3),
		protocol: Protocol::Grpc,
	};
	let provider = opentelemetry_otlp::new_pipeline()
		.metrics(opentelemetry_sdk::runtime::Tokio)
		.with_exporter(
			opentelemetry_otlp::new_exporter()
				.tonic()
				.with_export_config(export_config),
		)
		.with_period(Duration::from_secs(3))
		.with_timeout(Duration::from_secs(10))
		.build()?;

	global::set_meter_provider(provider);
	let meter = global::meter("avail_light_client");
	// Initialize counters - they need to persist unlike Gauges that are recreated on every record
	// TODO - move to a separate func once there's more than 1 counter
	let session_block_counter = meter.u64_counter("session_block_counter").init();
	Ok(OTMetrics {
		global_meter: meter,
		session_block_counter,
	})
}

pub fn record(event: OTMetricEvent, meter: &Meter, peer_id: &str) {
	match event {
		OTMetricEvent::SessionBlockCounter(counter) => {
			let peer_id = peer_id.to_string();
			counter.add(1, [KeyValue::new("peerID", peer_id.clone())].as_ref());
		},
		OTMetricEvent::TotalBlockNumber(num) => {
			let total_block_number = meter.u64_observable_gauge("total_block_number").init();
			let peer_id = peer_id.to_string();
			meter
				.register_callback(&[total_block_number.as_any()], move |observer| {
					observer.observe_u64(
						&total_block_number,
						num.into(),
						[KeyValue::new("peerID", peer_id.clone())].as_ref(),
					)
				})
				.unwrap();
		},
		OTMetricEvent::DHTFetched(num) => {
			let dht_fetched = meter.f64_observable_gauge("dht_fetched").init();
			let peer_id = peer_id.to_string();
			meter
				.register_callback(&[dht_fetched.as_any()], move |observer| {
					observer.observe_f64(
						&dht_fetched,
						num,
						[KeyValue::new("peerID", peer_id.clone())].as_ref(),
					)
				})
				.unwrap();
		},
		OTMetricEvent::DHTFetchedPercentage(num) => {
			let dht_fetched_percentage =
				meter.f64_observable_gauge("dht_fetched_percentage").init();
			let peer_id = peer_id.to_string();
			meter
				.register_callback(&[dht_fetched_percentage.as_any()], move |observer| {
					observer.observe_f64(
						&dht_fetched_percentage,
						num,
						[KeyValue::new("peerID", peer_id.clone())].as_ref(),
					)
				})
				.unwrap();
		},
		OTMetricEvent::NodeRPCFetched(num) => {
			let node_rpc_fetched = meter.f64_observable_gauge("node_rpc_fetched").init();
			let peer_id = peer_id.to_string();
			meter
				.register_callback(&[node_rpc_fetched.as_any()], move |observer| {
					observer.observe_f64(
						&node_rpc_fetched,
						num,
						[KeyValue::new("peerID", peer_id.clone())].as_ref(),
					)
				})
				.unwrap();
		},
		OTMetricEvent::BlockConfidence(num) => {
			let block_confidence = meter.f64_observable_gauge("block_confidence").init();
			let peer_id = peer_id.to_string();
			meter
				.register_callback(&[block_confidence.as_any()], move |observer| {
					observer.observe_f64(
						&block_confidence,
						num,
						[KeyValue::new("peerID", peer_id.clone())].as_ref(),
					)
				})
				.unwrap();
		},
		OTMetricEvent::RPCCallDuration(num) => {
			let rpc_call_duration = meter.f64_observable_gauge("rpc_call_duration").init();
			let peer_id = peer_id.to_string();
			meter
				.register_callback(&[rpc_call_duration.as_any()], move |observer| {
					observer.observe_f64(
						&rpc_call_duration,
						num.into(),
						[KeyValue::new("peerID", peer_id.clone())].as_ref(),
					)
				})
				.unwrap();
		},
		OTMetricEvent::DHTPutDuration(num) => {
			let dht_put_duration = meter.f64_observable_gauge("dht_put_duration").init();
			let peer_id = peer_id.to_string();
			meter
				.register_callback(&[dht_put_duration.as_any()], move |observer| {
					observer.observe_f64(
						&dht_put_duration,
						num,
						[KeyValue::new("peerID", peer_id.clone())].as_ref(),
					)
				})
				.unwrap();
		},
		OTMetricEvent::DHTPutSuccess(num) => {
			let dht_put_success = meter.f64_observable_gauge("dht_put_success").init();
			let peer_id = peer_id.to_string();
			meter
				.register_callback(&[dht_put_success.as_any()], move |observer| {
					observer.observe_f64(
						&dht_put_success,
						num,
						[KeyValue::new("peerID", peer_id.clone())].as_ref(),
					)
				})
				.unwrap();
		},
		OTMetricEvent::DHTPutRowsDuration(num) => {
			let peer_id = peer_id;
			let dht_put_rows_duration = meter.f64_observable_gauge("dht_put_rows_duration").init();
			let peer_id = peer_id.to_string();
			meter
				.register_callback(&[dht_put_rows_duration.as_any()], move |observer| {
					observer.observe_f64(
						&dht_put_rows_duration,
						num,
						[KeyValue::new("peerID", peer_id.clone())].as_ref(),
					)
				})
				.unwrap();
		},
		OTMetricEvent::DHTPutRowsSuccess(num) => {
			let dht_put_rows_success = meter.f64_observable_gauge("dht_put_rows_success").init();
			let peer_id = peer_id.to_string();
			meter
				.register_callback(&[dht_put_rows_success.as_any()], move |observer| {
					observer.observe_f64(
						&dht_put_rows_success,
						num,
						[KeyValue::new("peerID", peer_id.clone())].as_ref(),
					)
				})
				.unwrap();
		},
		OTMetricEvent::KadRoutingTablePeerNum(num) => {
			let kad_routing_table_peer_num = meter
				.u64_observable_gauge("kad_routing_table_peer_num")
				.init();
			let peer_id = peer_id.to_string();
			meter
				.register_callback(&[kad_routing_table_peer_num.as_any()], move |observer| {
					observer.observe_u64(
						&kad_routing_table_peer_num,
						num.into(),
						[KeyValue::new("peerID", peer_id.clone())].as_ref(),
					)
				})
				.unwrap();
		},
	}
}
