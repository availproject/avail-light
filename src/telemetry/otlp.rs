use anyhow::{Error, Ok, Result};
use opentelemetry_api::{
	global,
	metrics::{Counter, Meter},
	KeyValue,
};
use opentelemetry_otlp::{ExportConfig, Protocol, WithExportConfig};
use std::{sync::RwLock, time::Duration};

#[derive(Debug)]
pub struct Metrics {
	pub meter: Meter,
	pub session_block_counter: Counter<u64>,
	pub peer_id: String,
	pub multiaddress: RwLock<String>,
	pub ip: RwLock<String>,
	pub role: String,
}

impl Metrics {
	fn attributes(&self) -> [KeyValue; 6] {
		[
			KeyValue::new("job", "avail_light_client"),
			KeyValue::new("version", clap::crate_version!()),
			KeyValue::new("role", self.role.clone()),
			KeyValue::new("peerID", self.peer_id.clone()),
			KeyValue::new("multiaddress", self.multiaddress.read().unwrap().clone()),
			KeyValue::new("ip", self.ip.read().unwrap().clone()),
		]
	}

	fn record_u64(&self, name: &'static str, value: u64) -> Result<()> {
		let instrument = self.meter.u64_observable_gauge(name).try_init()?;
		let attributes = self.attributes();
		self.meter
			.register_callback(&[instrument.as_any()], move |observer| {
				observer.observe_u64(&instrument, value, &attributes)
			})?;
		Ok(())
	}

	fn record_f64(&self, name: &'static str, value: f64) -> Result<()> {
		let instrument = self.meter.f64_observable_gauge(name).try_init()?;
		let attributes = self.attributes();
		self.meter
			.register_callback(&[instrument.as_any()], move |observer| {
				observer.observe_f64(&instrument, value, &attributes)
			})?;
		Ok(())
	}
}

impl super::Metrics for Metrics {
	fn count(&self, counter: super::MetricCounter) {
		match counter {
			super::MetricCounter::SessionBlock => {
				self.session_block_counter.add(1, &self.attributes());
			},
		}
	}

	fn record(&self, value: super::MetricValue) -> Result<()> {
		match value {
			super::MetricValue::TotalBlockNumber(number) => {
				self.record_u64("total_block_number", number.into())?;
			},
			super::MetricValue::DHTFetched(number) => {
				self.record_f64("dht_fetched", number)?;
			},
			super::MetricValue::DHTFetchedPercentage(number) => {
				self.record_f64("dht_fetched_percentage", number)?;
			},
			super::MetricValue::NodeRPCFetched(number) => {
				self.record_f64("node_rpc_fetched", number)?;
			},
			super::MetricValue::BlockConfidence(number) => {
				self.record_f64("block_confidence", number)?;
			},
			super::MetricValue::RPCCallDuration(number) => {
				self.record_f64("rpc_call_duration", number)?;
			},
			super::MetricValue::DHTPutDuration(number) => {
				self.record_f64("dht_put_duration", number)?;
			},
			super::MetricValue::DHTPutSuccess(number) => {
				self.record_f64("dht_put_success", number)?;
			},
			super::MetricValue::DHTPutRowsDuration(number) => {
				self.record_f64("dht_put_rows_duration", number)?;
			},
			super::MetricValue::DHTPutRowsSuccess(number) => {
				self.record_f64("dht_put_rows_success", number)?;
			},
			super::MetricValue::KadRoutingTablePeerNum(number) => {
				self.record_u64("kad_routing_table_peer_num", number.into())?;
			},
		};
		Ok(())
	}
}

pub fn initialize(endpoint: String, peer_id: String, role: String) -> Result<Metrics, Error> {
	let export_config = ExportConfig {
		endpoint,
		timeout: Duration::from_secs(10),
		protocol: Protocol::Grpc,
	};
	let provider = opentelemetry_otlp::new_pipeline()
		.metrics(opentelemetry_sdk::runtime::Tokio)
		.with_exporter(
			opentelemetry_otlp::new_exporter()
				.tonic()
				.with_export_config(export_config),
		)
		.with_period(Duration::from_secs(10)) // Configures the intervening time between exports
		.with_timeout(Duration::from_secs(15)) // Configures the time a OT waits for an export to complete before canceling it.
		.build()?;

	global::set_meter_provider(provider);
	let meter = global::meter("avail_light_client");
	// Initialize counters - they need to persist unlike Gauges that are recreated on every record
	// TODO - move to a separate func once there's more than 1 counter
	let session_block_counter = meter.u64_counter("session_block_counter").init();
	Ok(Metrics {
		meter,
		session_block_counter,
		peer_id,
		multiaddress: RwLock::new("".to_string()), // Default value is empty until first processed block triggers an update
		ip: RwLock::new("".to_string()),
		role,
	})
}
