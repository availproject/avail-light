use async_trait::async_trait;
use color_eyre::Result;
use opentelemetry_api::{
	global,
	metrics::{Counter, Meter},
	KeyValue,
};
use opentelemetry_otlp::{ExportConfig, Protocol, WithExportConfig};
use std::{
	collections::HashMap,
	sync::{Arc, RwLock},
	time::{Duration, Instant},
};

use crate::types::{Origin, OtelConfig};

use super::MetricCounter;

const ATTRIBUTE_NUMBER: usize = 7;

#[derive(Debug)]
pub struct LastRecorded {
	counters: Instant,
	gauge_u64: Instant,
	gauge_f64: Instant,
}
#[derive(Debug)]
pub struct AggregatedMetrics {
	counter_sums: HashMap<String, u64>,
	gauge_last_value_u64: HashMap<String, u64>,
	// (f64, usize) = (average_value, count)
	gauge_averages_f64: HashMap<String, (f64, usize)>,
	last_recorded: LastRecorded,
	otel_flush_frequency_secs: u64,
}

#[derive(Debug)]
pub struct Metrics {
	meter: Meter,
	counters: HashMap<String, Counter<u64>>,
	attributes: MetricAttributes,
	aggregated_metrics: Arc<RwLock<AggregatedMetrics>>,
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

	// Used only by TotalBlockNumber and HealthCheck
	// The latest value is temporarily stored and periodically flushed to OTel
	async fn record_u64(&self, name: &'static str, value: u64) -> Result<()> {
		// Update with the latest value
		let mut aggregated_metrics = self.aggregated_metrics.write().unwrap();
		aggregated_metrics
			.gauge_last_value_u64
			.entry(name.to_string())
			.and_modify(|last_value| {
				*last_value = value;
			})
			.or_insert(value);

		// Flush all temporary aggregated gauges to OTel
		let now = Instant::now();
		if now.duration_since(aggregated_metrics.last_recorded.gauge_u64)
			>= Duration::from_secs(aggregated_metrics.otel_flush_frequency_secs)
		{
			for (gauge_name, last_value) in aggregated_metrics.gauge_last_value_u64.iter_mut() {
				let val = *last_value;
				let instrument = self
					.meter
					.u64_observable_gauge(gauge_name.clone())
					.try_init()?;
				let attributes = self.attributes();
				self.meter
					.register_callback(&[instrument.as_any()], move |observer| {
						observer.observe_u64(&instrument, val, &attributes)
					})?;
			}
			aggregated_metrics.last_recorded.gauge_u64 = Instant::now();
		}

		Ok(())
	}

	async fn record_f64(&self, name: &'static str, value: f64) -> Result<()> {
		// Update aggregate metrics by calculating running average
		let mut aggregated_metrics = self.aggregated_metrics.write().unwrap();
		aggregated_metrics
			.gauge_averages_f64
			.entry(name.to_string())
			.and_modify(|(avg, count)| {
				*avg = (*avg * *count as f64 + value) / (*count as f64 + 1.0);
				*count += 1;
			})
			.or_insert((value, 1));

		// Flush all temporary aggregated gauges to OTel
		let now = Instant::now();
		if now.duration_since(aggregated_metrics.last_recorded.gauge_f64)
			>= Duration::from_secs(aggregated_metrics.otel_flush_frequency_secs)
		{
			for (gauge_name, (average, count)) in aggregated_metrics.gauge_averages_f64.iter_mut() {
				let avg = *average;
				let instrument = self
					.meter
					.f64_observable_gauge(gauge_name.clone())
					.try_init()?;
				let attributes = self.attributes();
				self.meter
					.register_callback(&[instrument.as_any()], move |observer| {
						observer.observe_f64(&instrument, avg, &attributes)
					})?;
				*count = 0;
			}
			aggregated_metrics.last_recorded.gauge_f64 = Instant::now();
		}

		Ok(())
	}
}

#[async_trait]
impl super::Metrics for Metrics {
	async fn count(&self, counter: super::MetricCounter) {
		if counter.is_allowed(&self.attributes.origin) {
			let mut aggregated_metrics = self.aggregated_metrics.write().unwrap();
			aggregated_metrics
				.counter_sums
				.entry(counter.to_string())
				.and_modify(|count| {
					*count += 1;
				})
				.or_insert(1);

			// Flush all temporary aggregated counters to OTel
			let now = Instant::now();
			if now.duration_since(aggregated_metrics.last_recorded.counters)
				>= Duration::from_secs(aggregated_metrics.otel_flush_frequency_secs)
			{
				for (counter_name, count) in aggregated_metrics.counter_sums.iter_mut() {
					let cnt = *count;
					__self.counters[&counter_name.clone()].add(cnt, &__self.attributes());
					*count = 0;
				}
				aggregated_metrics.last_recorded.counters = Instant::now();
			}
		}
	}

	async fn record(&self, value: super::MetricValue) -> Result<()> {
		if value.is_allowed(&self.attributes.origin) {
			match value {
				super::MetricValue::TotalBlockNumber(number) => {
					self.record_u64("total_block_number", number.into()).await?;
				},
				super::MetricValue::DHTFetched(number) => {
					self.record_f64("dht_fetched", number).await?;
				},
				super::MetricValue::DHTFetchedPercentage(number) => {
					self.record_f64("dht_fetched_percentage", number).await?;
				},
				super::MetricValue::DHTFetchDuration(number) => {
					self.record_f64("dht_fetch_duration", number).await?;
				},
				super::MetricValue::NodeRPCFetched(number) => {
					self.record_f64("node_rpc_fetched", number).await?;
				},
				super::MetricValue::NodeRPCFetchDuration(number) => {
					self.record_f64("node_rpc_fetch_duration", number).await?;
				},
				super::MetricValue::BlockConfidence(number) => {
					self.record_f64("block_confidence", number).await?;
				},
				super::MetricValue::BlockConfidenceTreshold(number) => {
					self.record_f64("block_confidence_treshold", number).await?;
				},
				super::MetricValue::RPCCallDuration(number) => {
					self.record_f64("rpc_call_duration", number).await?;
				},
				super::MetricValue::DHTPutDuration(number) => {
					self.record_f64("dht_put_duration", number).await?;
				},
				super::MetricValue::DHTPutSuccess(number) => {
					self.record_f64("dht_put_success", number).await?;
				},
				super::MetricValue::ConnectedPeersNum(number) => {
					self.record_f64("connected_peers_num", number as f64)
						.await?;
				},
				super::MetricValue::HealthCheck() => {
					self.record_u64("up", 1).await?;
				},
				super::MetricValue::BlockProcessingDelay(number) => {
					self.record_f64("block_processing_delay", number).await?;
				},
				super::MetricValue::ReplicationFactor(number) => {
					self.record_f64("replication_factor", number as f64).await?;
				},
				super::MetricValue::QueryTimeout(number) => {
					self.record_f64("query_timeout", number as f64).await?;
				},
				super::MetricValue::PingLatency(number) => {
					self.record_f64("ping_latency", number).await?;
				},
				#[cfg(feature = "crawl")]
				super::MetricValue::CrawlCellsSuccessRate(number) => {
					self.record_f64("crawl_cells_success_rate", number).await?;
				},
				#[cfg(feature = "crawl")]
				super::MetricValue::CrawlRowsSuccessRate(number) => {
					self.record_f64("crawl_rows_success_rate", number).await?;
				},
				#[cfg(feature = "crawl")]
				super::MetricValue::CrawlBlockDelay(number) => {
					self.record_f64("crawl_block_delay", number).await?;
				},
			};
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
		aggregated_metrics: Arc::new(RwLock::new(AggregatedMetrics {
			counter_sums: Default::default(),
			last_recorded: LastRecorded {
				counters: Instant::now(),
				gauge_u64: Instant::now(),
				gauge_f64: Instant::now(),
			},
			gauge_last_value_u64: Default::default(),
			gauge_averages_f64: Default::default(),
			otel_flush_frequency_secs: ot_config.otel_flush_frequency_secs,
		})),
	})
}
