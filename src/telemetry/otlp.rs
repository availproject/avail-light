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

use crate::types::{Origin, OtelConfig};

use super::{MetricCounter, MetricValue};

const ATTRIBUTE_NUMBER: usize = 7;

// NOTE: Buffers are less space efficient, as opposed to the solution with in place compute.
// That can be optimized by using dedicated data structure with proper bounds .
#[derive(Debug)]
pub struct Metrics {
	meter: Meter,
	counters: HashMap<String, Counter<u64>>,
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

		match value {
			TotalBlockNumber(number) => MaxU64("total_block_number", number as u64),
			DHTFetched(number) => AvgF64("dht_fetched", number),
			DHTFetchedPercentage(number) => AvgF64("dht_fetched_percentage", number),
			DHTFetchDuration(number) => AvgF64("dht_fetch_duration", number),
			NodeRPCFetched(number) => AvgF64("node_rpc_fetched", number),
			NodeRPCFetchDuration(number) => AvgF64("node_rpc_fetch_duration", number),
			BlockConfidence(number) => AvgF64("block_confidence", number),
			BlockConfidenceThreshold(number) => AvgF64("block_confidence_threshold", number),
			RPCCallDuration(number) => AvgF64("rpc_call_duration", number),
			DHTPutDuration(number) => AvgF64("dht_put_duration", number),
			DHTPutSuccess(number) => AvgF64("dht_put_success", number),
			ConnectedPeersNum(number) => AvgF64("connected_peers_num", number as f64),
			HealthCheck() => MaxU64("up", 1),
			BlockProcessingDelay(number) => AvgF64("block_processing_delay", number),
			ReplicationFactor(number) => AvgF64("replication_factor", number as f64),
			QueryTimeout(number) => AvgF64("query_timeout", number as f64),
			PingLatency(number) => AvgF64("ping_latency", number),
			#[cfg(feature = "crawl")]
			CrawlCellsSuccessRate(number) => AvgF64("crawl_cells_success_rate", number),
			#[cfg(feature = "crawl")]
			CrawlRowsSuccessRate(number) => AvgF64("crawl_rows_success_rate", number),
			#[cfg(feature = "crawl")]
			CrawlBlockDelay(number) => AvgF64("crawl_block_delay", number),
		}
	}
}

/// Counts occurrences of counters in the provided buffer.
/// Returned value is a `HashMap` where the keys are the counter name,
/// and values are the counts of those counters.
fn flatten_counters(buffer: &[impl ToString]) -> HashMap<String, u64> {
	let mut result = HashMap::new();
	for counter in buffer {
		result
			.entry(counter.to_string())
			.and_modify(|count| *count += 1)
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
			self.counters[&counter.to_string()].add(1, &self.attributes());
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
	let counters = MetricCounter::init_counters(meter.clone(), origin);
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
		expected.insert("avail.light.starts".to_string(), 1);
		assert_eq!(one, expected);

		let two = flatten_counters(&[Starts, Starts]);
		let mut expected = HashMap::new();
		expected.insert("avail.light.starts".to_string(), 2);
		assert_eq!(two, expected);

		let buffer = vec![
			Starts,
			SessionBlockCounter,
			IncomingConnectionErrors,
			IncomingConnectionErrors,
			IncomingConnections,
			Starts,
			IncomingGetRecordCounter,
			IncomingPutRecordCounter,
			Starts,
		];
		let result = flatten_counters(&buffer);
		let mut expected = HashMap::new();
		expected.insert("avail.light.starts".to_string(), 3);
		expected.insert("session_block_counter".to_string(), 1);
		expected.insert("incoming_connection_errors".to_string(), 2);
		expected.insert("incoming_connections".to_string(), 1);
		expected.insert("incoming_get_record_counter".to_string(), 1);
		expected.insert("incoming_put_record_counter".to_string(), 1);
		assert_eq!(result, expected);
	}

	#[test]
	fn test_flatten_metrics() {
		let (u64_metrics, f64_metrics) = flatten_metrics(&[] as &[MetricValue]);
		assert!(u64_metrics.is_empty());
		assert!(f64_metrics.is_empty());

		let buffer = &[MetricValue::BlockConfidence(90.0)];
		let (u64_metrics, f64_metrics) = flatten_metrics(buffer);
		assert!(u64_metrics.is_empty());
		assert_eq!(f64_metrics.len(), 1);
		assert_eq!(*f64_metrics.get("block_confidence").unwrap(), 90.0);

		let buffer = &[
			MetricValue::BlockConfidence(90.0),
			MetricValue::TotalBlockNumber(1),
			MetricValue::BlockConfidence(93.0),
		];
		let (u64_metrics, f64_metrics) = flatten_metrics(buffer);
		assert_eq!(u64_metrics.len(), 1);
		assert_eq!(*u64_metrics.get("total_block_number").unwrap(), 1);
		assert_eq!(f64_metrics.len(), 1);
		assert_eq!(*f64_metrics.get("block_confidence").unwrap(), 91.5);

		let buffer = &[
			MetricValue::BlockConfidence(90.0),
			MetricValue::TotalBlockNumber(1),
			MetricValue::BlockConfidence(93.0),
			MetricValue::BlockConfidence(93.0),
			MetricValue::BlockConfidence(99.0),
			MetricValue::TotalBlockNumber(10),
			MetricValue::TotalBlockNumber(1),
		];
		let (u64_metrics, f64_metrics) = flatten_metrics(buffer);
		assert_eq!(u64_metrics.len(), 1);
		assert_eq!(*u64_metrics.get("total_block_number").unwrap(), 10);
		assert_eq!(f64_metrics.len(), 1);
		assert_eq!(*f64_metrics.get("block_confidence").unwrap(), 93.75);

		let buffer = &[
			MetricValue::ConnectedPeersNum(90),
			MetricValue::HealthCheck(),
			MetricValue::DHTFetchDuration(1.0),
			MetricValue::DHTPutSuccess(10.0),
			MetricValue::BlockConfidence(99.0),
			MetricValue::HealthCheck(),
			MetricValue::DHTFetchDuration(2.0),
			MetricValue::DHTFetchDuration(2.1),
			MetricValue::TotalBlockNumber(999),
			MetricValue::HealthCheck(),
			MetricValue::ConnectedPeersNum(80),
			MetricValue::BlockConfidence(98.0),
		];
		let (u64_metrics, f64_metrics) = flatten_metrics(buffer);
		assert_eq!(u64_metrics.len(), 2);
		assert_eq!(*u64_metrics.get("up").unwrap(), 1);
		assert_eq!(*u64_metrics.get("total_block_number").unwrap(), 999);
		assert_eq!(f64_metrics.len(), 4);
		assert_eq!(*f64_metrics.get("dht_put_success").unwrap(), 10.0);
		assert_eq!(*f64_metrics.get("dht_fetch_duration").unwrap(), 1.7);
		assert_eq!(*f64_metrics.get("block_confidence").unwrap(), 98.5);
		assert_eq!(*f64_metrics.get("connected_peers_num").unwrap(), 85.0);
	}
}
