use super::{MetricCounter, MetricValue, Value};
use crate::{
	telemetry::MetricName,
	types::{Origin, ProjectName},
};
use color_eyre::Result;
use opentelemetry::{
	global,
	metrics::{Counter, Meter},
	KeyValue,
};
use opentelemetry_otlp::{MetricExporter, Protocol, WithExportConfig};
use opentelemetry_sdk::{
	metrics::{PeriodicReader, SdkMeterProvider},
	runtime::Tokio,
	Resource,
};
use serde::{Deserialize, Serialize};
use std::{
	cmp,
	collections::HashMap,
	sync::{Arc, Mutex},
	time::Duration,
};
use tracing::trace;

type U64Gauges = HashMap<&'static str, (u64, Vec<KeyValue>)>;
type F64Gauges = HashMap<&'static str, (f64, u64, Vec<KeyValue>)>;
type Attributes = HashMap<&'static str, String>;

// NOTE: Buffers are less space efficient, as opposed to the solution with in place compute.
// That can be optimized by using dedicated data structure with proper bounds.
#[derive(Debug)]
pub struct Metrics {
	origin: Origin,
	counters: HashMap<&'static str, Counter<u64>>,
	u64_gauges: Arc<Mutex<U64Gauges>>,
	f64_gauges: Arc<Mutex<F64Gauges>>,
	attributes: Arc<Mutex<Attributes>>,
}

impl Metrics {
	pub fn set_attribute(&mut self, name: &'static str, value: String) {
		let mut attributes = self.attributes.lock().unwrap();
		attributes.insert(name, value);
	}

	fn attributes(&self) -> Vec<KeyValue> {
		let attributes = self.attributes.lock().unwrap();
		attributes
			.iter()
			.map(|(k, v)| KeyValue::new(*k, v.clone()))
			.collect()
	}

	/// Puts counter to the counter buffer if it is allowed.
	/// If counter is not buffered, counter is incremented.
	pub fn count(&mut self, counter: super::MetricCounter) {
		self.count_n(counter, 1);
	}

	pub fn count_n(&mut self, counter: super::MetricCounter, value: u64) {
		if !counter.is_allowed(&self.origin) {
			return;
		}
		self.counters[&counter.name()].add(value, &self.attributes());
	}

	/// Puts metric to the metric buffer if it is allowed.
	pub fn record<T>(&mut self, value: T)
	where
		T: Value + Into<Record> + Send,
	{
		if !value.is_allowed(&self.origin) {
			return;
		}

		let record: Record = value.into();
		match record {
			Record::MaxU64(name, value) => {
				let mut u64_gauges = self.u64_gauges.lock().unwrap();
				u64_gauges
					.entry(name)
					.and_modify(|(old, _)| *old = cmp::max(*old, value))
					.or_insert((value, self.attributes()));
			},
			Record::AvgF64(name, value) => {
				let mut f64_gauges = self.f64_gauges.lock().unwrap();
				f64_gauges
					.entry(name)
					.and_modify(|(average, n, _)| {
						*n += 1;
						*average += (value - *average) / *n as f64;
					})
					.or_insert((value, 1, self.attributes()));
			},
		}
	}
}

#[derive(Debug)]
pub enum Record {
	MaxU64(&'static str, u64),
	AvgF64(&'static str, f64),
}

impl From<MetricValue> for Record {
	fn from(value: MetricValue) -> Self {
		use MetricValue::*;
		use Record::*;

		let name = value.name();

		match value {
			BlockHeight(number) => MaxU64(name, number as u64),
			BlockConfidence(number) => AvgF64(name, number),
			BlockConfidenceThreshold(number) => AvgF64(name, number),
			BlockProcessingDelay(number) => AvgF64(name, number),

			DHTReplicationFactor(number) => AvgF64(name, number as f64),

			DHTFetched(number) => AvgF64(name, number),
			DHTFetchedPercentage(number) => AvgF64(name, number),
			DHTFetchDuration(number) => AvgF64(name, number),
			DHTPutDuration(number) => AvgF64(name, number),
			DHTPutSuccess(number) => AvgF64(name, number),

			DHTConnectedPeers(number) => AvgF64(name, number as f64),
			DHTQueryTimeout(number) => AvgF64(name, number as f64),
			DHTPingLatency(number) => AvgF64(name, number),

			RPCFetched(number) => AvgF64(name, number),
			RPCFetchDuration(number) => AvgF64(name, number),
			RPCCallDuration(number) => AvgF64(name, number),
		}
	}
}

fn init_counters(
	meter: Meter,
	origin: &Origin,
	project_name: ProjectName,
) -> HashMap<&'static str, Counter<u64>> {
	MetricCounter::default_values()
		.iter()
		.filter(|counter| MetricCounter::is_allowed(counter, origin))
		.map(|counter| {
			let otel_counter_name = format!("{project_name}.{name}", name = counter.name());
			// Keep the `static str as the local buffer map key, but change the OTel counter name`
			(counter.name(), meter.u64_counter(otel_counter_name).build())
		})
		.collect()
}

fn init_gauges(
	meter: Meter,
	origin: &Origin,
	project_name: ProjectName,
) -> (Arc<Mutex<U64Gauges>>, Arc<Mutex<F64Gauges>>) {
	let u64_gauges: Arc<Mutex<U64Gauges>> = Default::default();
	let f64_gauges: Arc<Mutex<F64Gauges>> = Default::default();

	for value in MetricValue::default_values() {
		if !value.is_allowed(origin) {
			continue;
		}

		match value.into() {
			Record::MaxU64(name, _) => {
				let gauge_name = project_name.gauge_name(name);
				let u64_gauges = u64_gauges.clone();

				meter
					.u64_observable_gauge(gauge_name.clone())
					.with_callback(move |observer| {
						let mut u64_gauges = u64_gauges.lock().unwrap();
						if let Some((value, attributes)) = u64_gauges.get(name) {
							trace!("Observed gauge: {gauge_name}, {value}, {attributes:?}");
							observer.observe(*value, &attributes.clone());
						};
						u64_gauges.remove(name);
					})
					.build();
			},
			Record::AvgF64(name, _) => {
				let gauge_name = project_name.gauge_name(name);
				let f64_gauges = f64_gauges.clone();

				meter
					.f64_observable_gauge(gauge_name.clone())
					.with_callback(move |observer| {
						let mut f64_gauges = f64_gauges.lock().unwrap();
						if let Some((value, _, attributes)) = f64_gauges.get(name) {
							trace!("Observed gauge: {gauge_name}, {value}, {attributes:?}");
							observer.observe(*value, &attributes.clone());
						};
						f64_gauges.remove(name);
					})
					.build();
			},
		}
	}

	(u64_gauges, f64_gauges)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct OtelConfig {
	/// OpenTelemetry Collector endpoint (default: `http://otelcollector.avail.tools:4317`)
	pub ot_collector_endpoint: String,
	pub ot_export_period: u64,
	pub ot_export_timeout: u64,
}

impl Default for OtelConfig {
	fn default() -> Self {
		Self {
			ot_collector_endpoint: "http://127.0.0.1:4317".to_string(),
			ot_export_period: 300,
			ot_export_timeout: 10,
		}
	}
}

pub fn initialize(
	project_name: ProjectName,
	origin: &Origin,
	ot_config: OtelConfig,
	resource_attributes: Vec<(&'static str, String)>,
) -> Result<Metrics> {
	let exporter = MetricExporter::builder()
		.with_tonic()
		.with_endpoint(&ot_config.ot_collector_endpoint)
		.with_protocol(Protocol::Grpc)
		.with_timeout(Duration::from_secs(ot_config.ot_export_timeout))
		.build()?;

	let reader = PeriodicReader::builder(exporter, Tokio)
		.with_interval(Duration::from_secs(ot_config.ot_export_period)) // Export interval
		.with_timeout(Duration::from_secs(ot_config.ot_export_timeout)) // Timeout for each export
		.build();

	let service_name = KeyValue::new("service.name", project_name.to_string());

	let mut resource = resource_attributes
		.iter()
		.map(|(k, v)| KeyValue::new(*k, v.clone()))
		.collect::<Vec<_>>();

	resource.push(service_name);

	let provider = SdkMeterProvider::builder()
		.with_reader(reader)
		.with_resource(Resource::new(resource))
		.build();

	global::set_meter_provider(provider);
	let meter = global::meter("avail_light_client");

	// Initialize counters - they need to persist unlike Gauges that are recreated on every record
	let counters = init_counters(meter.clone(), origin, project_name.clone());
	let (u64_gauges, f64_gauges) = init_gauges(meter, origin, project_name.clone());
	Ok(Metrics {
		origin: origin.clone(),
		counters,
		u64_gauges,
		f64_gauges,
		attributes: Default::default(),
	})
}
