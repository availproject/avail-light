use super::{MetricCounter, MetricValue};
use crate::{
	telemetry::MetricName,
	types::{Origin, OtelConfig},
};
use async_trait::async_trait;
use color_eyre::Result;
use opentelemetry_api::{
	global,
	metrics::{Counter, Meter},
	KeyValue,
};
use opentelemetry_otlp::{ExportConfig, Protocol, WithExportConfig};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;

const ATTRIBUTE_NUMBER: usize = 8;

// NOTE: Buffers are less space efficient, as opposed to the solution with in place compute.
// That can be optimized by using dedicated data structure with proper bounds.
#[derive(Debug)]
pub struct Metrics {
	meter: Meter,
	counters: HashMap<&'static str, Counter<u64>>,
	attributes: MetricAttributes,
	metric_buffer: Arc<Mutex<Vec<MetricValue>>>,
	counter_buffer: Arc<Mutex<Vec<MetricCounter>>>,
}

#[derive(Debug)]
pub struct MetricAttributes {
	pub role: String,
	pub peer_id: String,
	pub origin: Origin,
	pub avail_address: String,
	pub operating_mode: String,
	pub partition_size: String,
	pub network: String,
}

impl Metrics {
	fn attributes(&self) -> [KeyValue; ATTRIBUTE_NUMBER] {
		[
			KeyValue::new("version", clap::crate_version!()),
			KeyValue::new("role", self.attributes.role.clone()),
			KeyValue::new("origin", self.attributes.origin.to_string()),
			KeyValue::new("peerID", self.attributes.peer_id.clone()),
			KeyValue::new("avail_address", self.attributes.avail_address.clone()),
			KeyValue::new("partition_size", self.attributes.partition_size.clone()),
			KeyValue::new("operating_mode", self.attributes.operating_mode.clone()),
			KeyValue::new("network", self.attributes.network.clone()),
		]
	}

	async fn record_u64(&self, name: &'static str, value: u64) -> Result<()> {
		let instrument = self.meter.u64_observable_gauge(name).try_init()?;
		let attributes = self.attributes();
		self.meter
			.register_callback(&[instrument.as_any()], move |observer| {
				observer.observe_u64(&instrument, value, &attributes)
			})?;
		Ok(())
	}

	async fn record_f64(&self, name: &'static str, value: f64) -> Result<()> {
		let instrument = self.meter.f64_observable_gauge(name).try_init()?;
		let attributes = self.attributes();
		self.meter
			.register_callback(&[instrument.as_any()], move |observer| {
				observer.observe_f64(&instrument, value, &attributes)
			})?;
		Ok(())
	}
}

enum Record {
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

			#[cfg(feature = "crawl")]
			CrawlCellsSuccessRate(number) => AvgF64(name, number),
			#[cfg(feature = "crawl")]
			CrawlRowsSuccessRate(number) => AvgF64(name, number),
			#[cfg(feature = "crawl")]
			CrawlBlockDelay(number) => AvgF64(name, number),
		}
	}
}

/// Counts occurrences of counters in the provided buffer.
/// Returned value is a `HashMap` where the keys are the counter name,
/// and values are the counts of those counters.
fn flatten_counters(buffer: &[MetricCounter]) -> HashMap<&'static str, u64> {
	let mut result = HashMap::new();
	for counter in buffer {
		result
			.entry(counter.name())
			.and_modify(|count| {
				if !counter.as_last() {
					*count += 1
				}
			})
			.or_insert(1);
	}
	result
}

/// Aggregates buffered metrics into `u64` or `f64` values, depending on the metric.
/// Returned values are a `HashMap`s where the keys are the metric name,
/// and values are the aggregations (avg, max, etc.) of those metrics.
fn flatten_metrics(
	buffer: &[impl Into<Record> + Clone],
) -> (HashMap<&'static str, u64>, HashMap<&'static str, f64>) {
	let mut u64_maximums: HashMap<&'static str, Vec<u64>> = HashMap::new();
	let mut f64_averages: HashMap<&'static str, Vec<f64>> = HashMap::new();

	for value in buffer {
		match value.clone().into() {
			Record::MaxU64(name, number) => u64_maximums.entry(name).or_default().push(number),
			Record::AvgF64(name, number) => f64_averages.entry(name).or_default().push(number),
		}
	}

	let u64_metrics = u64_maximums
		.into_iter()
		.map(|(name, v)| (name, v.into_iter().max().unwrap_or(0)))
		.collect();

	let f64_metrics = f64_averages
		.into_iter()
		.map(|(name, v)| (name, v.iter().sum::<f64>() / v.len() as f64))
		.collect();

	(u64_metrics, f64_metrics)
}

#[async_trait]
impl super::Metrics for Metrics {
	/// Puts counter to the counter buffer if it is allowed.
	/// If counter is not buffered, counter is incremented.
	async fn count(&self, counter: super::MetricCounter) {
		if !counter.is_allowed(&self.attributes.origin) {
			return;
		}
		if !counter.is_buffered() {
			self.counters[&counter.name()].add(1, &self.attributes());
			return;
		}
		let mut counter_buffer = self.counter_buffer.lock().await;
		counter_buffer.push(counter);
	}

	/// Puts metric to the metric buffer if it is allowed.
	async fn record(&self, value: super::MetricValue) {
		if !value.is_allowed(&self.attributes.origin) {
			return;
		}

		let mut metric_buffer = self.metric_buffer.lock().await;
		metric_buffer.push(value);
	}

	/// Calculates counters and average metrics, and flushes buffers to the collector.
	async fn flush(&self) -> Result<()> {
		let mut counter_buffer = self.counter_buffer.lock().await;
		let counters = flatten_counters(&counter_buffer);
		counter_buffer.clear();

		let mut metric_buffer = self.metric_buffer.lock().await;
		let (metrics_u64, metrics_f64) = flatten_metrics(&metric_buffer);
		metric_buffer.clear();

		for (counter, value) in counters {
			self.counters[&counter].add(value, &self.attributes());
		}

		// TODO: Aggregate errors instead of early return
		for (metric, value) in metrics_u64.into_iter() {
			self.record_u64(metric, value).await?;
		}

		for (metric, value) in metrics_f64.into_iter() {
			self.record_f64(metric, value).await?;
		}

		Ok(())
	}
}

fn init_counters(meter: Meter, origin: Origin) -> HashMap<&'static str, Counter<u64>> {
	[
		MetricCounter::Starts,
		MetricCounter::SessionBlocks,
		MetricCounter::OutgoingConnectionErrors,
		MetricCounter::IncomingConnectionErrors,
		MetricCounter::IncomingConnections,
		MetricCounter::EstablishedConnections,
		MetricCounter::IncomingPutRecord,
		MetricCounter::IncomingGetRecord,
	]
	.iter()
	.filter(|counter| MetricCounter::is_allowed(counter, &origin))
	.map(|counter| (counter.name(), meter.u64_counter(counter.name()).init()))
	.collect()
}

pub fn initialize(
	endpoint: String,
	attributes: MetricAttributes,
	origin: Origin,
	ot_config: OtelConfig,
) -> Result<Metrics> {
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
		.with_period(Duration::from_secs(ot_config.ot_export_period)) // Configures the intervening time between exports
		.with_timeout(Duration::from_secs(ot_config.ot_export_timeout)) // Configures the time a OT waits for an export to complete before canceling it.
		.build()?;

	global::set_meter_provider(provider);
	let meter = global::meter("avail_light_client");

	// Initialize counters - they need to persist unlike Gauges that are recreated on every record
	let counters = init_counters(meter.clone(), origin);
	Ok(Metrics {
		meter,
		attributes,
		counters,
		metric_buffer: Arc::new(Mutex::new(vec![])),
		counter_buffer: Arc::new(Mutex::new(vec![])),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_flatten_counters() {
		use MetricCounter::*;
		// Empty buffer
		assert!(flatten_counters(&[] as &[MetricCounter]).is_empty());

		let one = flatten_counters(&[Starts]);
		let mut expected = HashMap::new();
		expected.insert(Starts.name(), 1);
		assert_eq!(one, expected);

		let two = flatten_counters(&[Starts, Starts]);
		let mut expected = HashMap::new();
		expected.insert(Starts.name(), 2);
		assert_eq!(two, expected);

		let buffer = vec![
			Starts,
			Up,
			SessionBlocks,
			IncomingConnectionErrors,
			IncomingConnectionErrors,
			IncomingConnections,
			Up,
			Starts,
			IncomingGetRecord,
			Up,
			IncomingPutRecord,
			Starts,
		];
		let result = flatten_counters(&buffer);
		let mut expected = HashMap::new();
		expected.insert(Starts.name(), 3);
		expected.insert(Up.name(), 1);
		expected.insert(SessionBlocks.name(), 1);
		expected.insert(IncomingConnectionErrors.name(), 2);
		expected.insert(IncomingConnections.name(), 1);
		expected.insert(IncomingGetRecord.name(), 1);
		expected.insert(IncomingPutRecord.name(), 1);
		assert_eq!(result, expected);
	}

	#[test]
	fn test_flatten_metrics() {
		let (m_u64, m_f64) = flatten_metrics(&[] as &[MetricValue]);
		assert!(m_u64.is_empty());
		assert!(m_f64.is_empty());

		let buffer = &[MetricValue::BlockConfidence(90.0)];
		let (m_u64, m_f64) = flatten_metrics(buffer);
		assert!(m_u64.is_empty());
		assert_eq!(m_f64.len(), 1);
		assert_eq!(m_f64.get("avail.light.block.confidence"), Some(&90.0));

		let buffer = &[
			MetricValue::BlockConfidence(90.0),
			MetricValue::BlockHeight(1),
			MetricValue::BlockConfidence(93.0),
		];
		let (m_u64, m_f64) = flatten_metrics(buffer);
		assert_eq!(m_u64.len(), 1);
		assert_eq!(m_u64.get("avail.light.block.height"), Some(&1));
		assert_eq!(m_f64.len(), 1);
		assert_eq!(m_f64.get("avail.light.block.confidence"), Some(&91.5));

		let buffer = &[
			MetricValue::BlockConfidence(90.0),
			MetricValue::BlockHeight(1),
			MetricValue::BlockConfidence(93.0),
			MetricValue::BlockConfidence(93.0),
			MetricValue::BlockConfidence(99.0),
			MetricValue::BlockHeight(10),
			MetricValue::BlockHeight(1),
		];
		let (m_u64, m_f64) = flatten_metrics(buffer);
		assert_eq!(m_u64.len(), 1);
		assert_eq!(m_u64.get("avail.light.block.height"), Some(&10));
		assert_eq!(m_f64.len(), 1);
		assert_eq!(m_f64.get("avail.light.block.confidence"), Some(&93.75));

		let buffer = &[
			MetricValue::DHTConnectedPeers(90),
			MetricValue::DHTFetchDuration(1.0),
			MetricValue::DHTPutSuccess(10.0),
			MetricValue::BlockConfidence(99.0),
			MetricValue::DHTFetchDuration(2.0),
			MetricValue::DHTFetchDuration(2.1),
			MetricValue::BlockHeight(999),
			MetricValue::DHTConnectedPeers(80),
			MetricValue::BlockConfidence(98.0),
		];
		let (m_u64, m_f64) = flatten_metrics(buffer);
		assert_eq!(m_u64.len(), 1);
		assert_eq!(m_u64.get("avail.light.block.height"), Some(&999));
		assert_eq!(m_f64.len(), 4);
		assert_eq!(m_f64.get("avail.light.dht.put_success"), Some(&10.0));
		assert_eq!(m_f64.get("avail.light.dht.fetch_duration"), Some(&1.7));
		assert_eq!(m_f64.get("avail.light.block.confidence"), Some(&98.5));
		assert_eq!(m_f64.get("avail.light.dht.connected_peers"), Some(&85.0));
	}
}
