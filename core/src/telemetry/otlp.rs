use super::{metric, MetricCounter, MetricValue};
use crate::{telemetry::MetricName, types::Origin};
use async_trait::async_trait;
use color_eyre::{eyre::eyre, Result};
use libp2p::{
	kad::{Mode, QueryStats, RecordKey},
	Multiaddr,
};
use opentelemetry::{
	global,
	metrics::{Counter, Meter},
	KeyValue,
};
use opentelemetry_otlp::{ExportConfig, Protocol, WithExportConfig};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct BlockStat {
	pub total_count: usize,
	pub remaining_counter: usize,
	pub success_counter: usize,
	pub error_counter: usize,
	pub time_stat: u64,
}

impl BlockStat {
	pub fn increase_block_stat_counters(&mut self, cell_number: usize) {
		self.total_count += cell_number;
		self.remaining_counter += cell_number;
	}

	fn increment_success_counter(&mut self) {
		self.success_counter += 1;
	}

	fn increment_error_counter(&mut self) {
		self.error_counter += 1;
	}

	fn decrement_remaining_counter(&mut self) {
		self.remaining_counter -= 1;
	}

	fn is_completed(&self) -> bool {
		self.remaining_counter == 0
	}

	fn update_time_stat(&mut self, stats: &QueryStats) {
		self.time_stat = stats
			.duration()
			.as_ref()
			.map(Duration::as_secs)
			.unwrap_or_default();
	}

	fn success_rate(&self) -> f64 {
		self.success_counter as f64 / self.total_count as f64
	}
}

#[derive(PartialEq, Debug)]
enum DHTKey {
	Cell(u32, u32, u32),
	Row(u32, u32),
}

impl TryFrom<RecordKey> for DHTKey {
	type Error = color_eyre::Report;

	fn try_from(key: RecordKey) -> std::result::Result<Self, Self::Error> {
		match *String::from_utf8(key.to_vec())?
			.split(':')
			.map(str::parse::<u32>)
			.collect::<std::result::Result<Vec<_>, _>>()?
			.as_slice()
		{
			[block_num, row_num] => Ok(DHTKey::Row(block_num, row_num)),
			[block_num, row_num, col_num] => Ok(DHTKey::Cell(block_num, row_num, col_num)),
			_ => Err(eyre!("Invalid DHT key")),
		}
	}
}

// NOTE: Buffers are less space efficient, as opposed to the solution with in place compute.
// That can be optimized by using dedicated data structure with proper bounds.
#[derive(Debug)]
pub struct Metrics {
	meter: Meter,
	project_name: String,
	origin: Origin,
	mode: Mode,
	multiaddress: Multiaddr,
	counters: HashMap<&'static str, Counter<u64>>,
	attributes: Vec<KeyValue>,
	metric_buffer: Vec<Record>,
	counter_buffer: Vec<MetricCounter>,
	active_blocks: HashMap<u32, BlockStat>,
}

impl Metrics {
	fn attributes(&self) -> Vec<KeyValue> {
		let mut attributes = vec![
			KeyValue::new("origin", self.origin.to_string()),
			KeyValue::new("operating_mode", self.mode.to_string()),
			KeyValue::new("multiaddress", self.multiaddress.to_string()),
		];
		attributes.extend(self.attributes.clone());
		attributes
	}

	fn record_u64(&self, name: &'static str, value: u64) -> Result<()> {
		let gauge_name = format!("{}.{}", name, self.project_name);
		let instrument = self.meter.u64_observable_gauge(gauge_name).try_init()?;
		let attributes = self.attributes();
		self.meter
			.register_callback(&[instrument.as_any()], move |observer| {
				observer.observe_u64(&instrument, value, &attributes)
			})?;
		Ok(())
	}

	fn record_f64(&self, name: &'static str, value: f64) -> Result<()> {
		let gauge_name = format!("{}.{}", name, self.project_name);
		let instrument = self.meter.f64_observable_gauge(gauge_name).try_init()?;
		let attributes = self.attributes();
		self.meter
			.register_callback(&[instrument.as_any()], move |observer| {
				observer.observe_f64(&instrument, value, &attributes)
			})?;
		Ok(())
	}

	fn extract_block_num(&self, key: RecordKey) -> Result<u32> {
		key.try_into()
			.map(|dht_key| match dht_key {
				DHTKey::Cell(block_num, _, _) | DHTKey::Row(block_num, _) => block_num,
			})
			.map_err(|error| {
				warn!("Unable to cast KAD key to DHT key: {error}");
				eyre!("Invalid key: {error}")
			})
	}

	fn get_block_stat(&mut self, block_num: u32) -> Result<&mut BlockStat> {
		self.active_blocks
			.get_mut(&block_num)
			.ok_or_else(|| eyre!("Can't find block: {} in active block list", block_num))
	}

	fn log_and_record_metrics(&mut self, block_num: u32, success_rate: f64, time_stat: f64) {
		info!(
			"Cell upload success rate for block {}: {}. Duration: {}",
			block_num, success_rate, time_stat
		);

		super::Metrics::record(self, MetricValue::DHTPutSuccess(success_rate));
		super::Metrics::record(self, MetricValue::DHTPutDuration(time_stat));
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
fn flatten_metrics(buffer: &[Record]) -> (HashMap<&'static str, u64>, HashMap<&'static str, f64>) {
	let mut u64_maximums: HashMap<&'static str, Vec<u64>> = HashMap::new();
	let mut f64_averages: HashMap<&'static str, Vec<f64>> = HashMap::new();

	for value in buffer {
		match value {
			Record::MaxU64(name, number) => u64_maximums.entry(name).or_default().push(*number),
			Record::AvgF64(name, number) => f64_averages.entry(name).or_default().push(*number),
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
	fn count(&mut self, counter: super::MetricCounter) {
		if !counter.is_allowed(&self.origin) {
			return;
		}
		if !counter.is_buffered() {
			self.counters[&counter.name()].add(1, &self.attributes());
			return;
		}
		self.counter_buffer.push(counter);
	}

	/// Puts metric to the metric buffer if it is allowed.
	fn record<T>(&mut self, value: T)
	where
		T: metric::Value + Into<Record> + Send,
	{
		if !value.is_allowed(&self.origin) {
			return;
		}

		self.metric_buffer.push(value.into());
	}

	/// Calculates counters and average metrics, and flushes buffers to the collector.
	fn flush(&mut self) -> Result<()> {
		let counters = flatten_counters(&self.counter_buffer);
		self.counter_buffer.clear();

		let (metrics_u64, metrics_f64) = flatten_metrics(&self.metric_buffer);
		self.metric_buffer.clear();

		let attributes = self.attributes();
		for (counter, value) in counters {
			self.counters[&counter].add(value, &attributes);
		}

		// TODO: Aggregate errors instead of early return
		for (metric, value) in metrics_u64.into_iter() {
			self.record_u64(metric, value)?;
		}

		for (metric, value) in metrics_f64.into_iter() {
			self.record_f64(metric, value)?;
		}

		Ok(())
	}

	fn update_multiaddress(&mut self, value: Multiaddr) {
		self.multiaddress = value;
	}

	fn update_operating_mode(&mut self, value: Mode) {
		self.mode = value;
	}

	fn handle_new_put_record(&mut self, block_num: u32, records: Vec<libp2p::kad::Record>) {
		self.active_blocks
			.entry(block_num)
			.and_modify(|b| b.increase_block_stat_counters(records.len()))
			.or_insert(BlockStat {
				total_count: records.len(),
				remaining_counter: records.len(),
				success_counter: 0,
				error_counter: 0,
				time_stat: 0,
			});
	}

	fn handle_successful_put_record(
		&mut self,
		record_key: RecordKey,
		query_stats: QueryStats,
	) -> Result<()> {
		let block_num = self.extract_block_num(record_key)?;
		let block = self.get_block_stat(block_num)?;

		block.increment_success_counter();
		block.decrement_remaining_counter();
		block.update_time_stat(&query_stats);

		if block.is_completed() {
			let success_rate = block.success_rate();
			let time_stat = block.time_stat as f64;

			self.log_and_record_metrics(block_num, success_rate, time_stat);
		}

		Ok(())
	}

	fn handle_failed_put_record(
		&mut self,
		record_key: RecordKey,
		query_stats: QueryStats,
	) -> Result<()> {
		let block_num = self.extract_block_num(record_key)?;
		let block = self.get_block_stat(block_num)?;

		block.increment_error_counter();
		block.decrement_remaining_counter();
		block.update_time_stat(&query_stats);

		if block.is_completed() {
			let success_rate = block.success_rate();
			let time_stat = block.time_stat as f64;

			self.log_and_record_metrics(block_num, success_rate, time_stat);
		}

		Ok(())
	}
}

fn init_counters(
	meter: Meter,
	origin: &Origin,
	project_name: String,
) -> HashMap<&'static str, Counter<u64>> {
	[
		MetricCounter::Starts,
		MetricCounter::Up,
		MetricCounter::SessionBlocks,
		MetricCounter::OutgoingConnectionErrors,
		MetricCounter::IncomingConnectionErrors,
		MetricCounter::IncomingConnections,
		MetricCounter::EstablishedConnections,
		MetricCounter::IncomingPutRecord,
		MetricCounter::IncomingGetRecord,
		MetricCounter::EventLoopEvent,
	]
	.iter()
	.filter(|counter| MetricCounter::is_allowed(counter, origin))
	.map(|counter| {
		let otel_counter_name = format!("{}.{}", project_name, counter.name());
		// Keep the `static str as the local bufer map key, but change the OTel counter name`
		(counter.name(), meter.u64_counter(otel_counter_name).init())
	})
	.collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct OtelConfig {
	/// OpenTelemetry Collector endpoint (default: `http://otelcollector.avail.tools:4317`)
	pub ot_collector_endpoint: String,
	pub ot_export_period: u64,
	pub ot_export_timeout: u64,
	pub ot_flush_block_interval: u32,
}

impl Default for OtelConfig {
	fn default() -> Self {
		Self {
			ot_collector_endpoint: "http://127.0.0.1:4317".to_string(),
			ot_export_period: 300,
			ot_export_timeout: 10,
			ot_flush_block_interval: 15,
		}
	}
}

pub fn initialize(
	attributes: Vec<(&str, String)>,
	project_name: String,
	origin: &Origin,
	mode: Mode,
	ot_config: OtelConfig,
) -> Result<Metrics> {
	let export_config = ExportConfig {
		endpoint: ot_config.ot_collector_endpoint,
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

	let attributes = attributes
		.into_iter()
		.map(|(k, v)| KeyValue::new(k.to_string(), v))
		.collect();

	// Initialize counters - they need to persist unlike Gauges that are recreated on every record
	let counters = init_counters(meter.clone(), origin, project_name.clone());
	Ok(Metrics {
		meter,
		project_name,
		origin: origin.clone(),
		mode,
		multiaddress: Multiaddr::empty(),
		attributes,
		counters,
		metric_buffer: vec![],
		counter_buffer: vec![],
		active_blocks: Default::default(),
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

	fn flatten_metrics(
		values: Vec<MetricValue>,
	) -> (HashMap<&'static str, u64>, HashMap<&'static str, f64>) {
		super::flatten_metrics(&values.into_iter().map(Into::into).collect::<Vec<Record>>())
	}

	#[test]
	fn test_flatten_metrics() {
		let (m_u64, m_f64) = flatten_metrics(vec![]);
		assert!(m_u64.is_empty());
		assert!(m_f64.is_empty());

		let buffer = vec![MetricValue::BlockConfidence(90.0)];
		let (m_u64, m_f64) = flatten_metrics(buffer);
		assert!(m_u64.is_empty());
		assert_eq!(m_f64.len(), 1);
		assert_eq!(m_f64.get("light.block.confidence"), Some(&90.0));

		let buffer = vec![
			MetricValue::BlockConfidence(90.0),
			MetricValue::BlockHeight(1),
			MetricValue::BlockConfidence(93.0),
		];
		let (m_u64, m_f64) = flatten_metrics(buffer);
		assert_eq!(m_u64.len(), 1);
		assert_eq!(m_u64.get("light.block.height"), Some(&1));
		assert_eq!(m_f64.len(), 1);
		assert_eq!(m_f64.get("light.block.confidence"), Some(&91.5));

		let buffer = vec![
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
		assert_eq!(m_u64.get("light.block.height"), Some(&10));
		assert_eq!(m_f64.len(), 1);
		assert_eq!(m_f64.get("light.block.confidence"), Some(&93.75));

		let buffer = vec![
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
		assert_eq!(m_u64.get("light.block.height"), Some(&999));
		assert_eq!(m_f64.len(), 4);
		assert_eq!(m_f64.get("light.dht.put_success"), Some(&10.0));
		assert_eq!(m_f64.get("light.dht.fetch_duration"), Some(&1.7));
		assert_eq!(m_f64.get("light.block.confidence"), Some(&98.5));
		assert_eq!(m_f64.get("light.dht.connected_peers"), Some(&85.0));
	}
}
