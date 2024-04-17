use async_trait::async_trait;
use color_eyre::Result;
use opentelemetry_api::{
	global,
	metrics::{Counter, Meter},
	KeyValue,
};
use opentelemetry_otlp::{ExportConfig, Protocol, WithExportConfig};
use std::{collections::HashMap, time::Duration};

use crate::types::Origin;

use super::MetricCounter;

const ATTRIBUTE_NUMBER: usize = 7;

#[derive(Debug)]
pub struct Metrics {
	meter: Meter,
	counters: HashMap<String, Counter<u64>>,
	attributes: MetricAttributes,
	detailed_metrics: bool,
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
	async fn attributes(&self) -> [KeyValue; ATTRIBUTE_NUMBER] {
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
		let attributes = self.attributes().await;
		self.meter
			.register_callback(&[instrument.as_any()], move |observer| {
				observer.observe_u64(&instrument, value, &attributes)
			})?;
		Ok(())
	}

	async fn record_f64(&self, name: &'static str, value: f64) -> Result<()> {
		let instrument = self.meter.f64_observable_gauge(name).try_init()?;
		let attributes = self.attributes().await;
		self.meter
			.register_callback(&[instrument.as_any()], move |observer| {
				observer.observe_f64(&instrument, value, &attributes)
			})?;
		Ok(())
	}
}

#[async_trait]
impl super::Metrics for Metrics {
	async fn count(&self, counter: super::MetricCounter) {
		if counter.is_allowed(self.detailed_metrics) {
			__self.counters[&counter.to_string()].add(1, &__self.attributes().await);
		}
	}

	async fn record(&self, value: super::MetricValue) -> Result<()> {
		if value.is_allowed(self.detailed_metrics) {
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
					self.record_u64("connected_peers_num", number as u64)
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
) -> Result<Metrics> {
	// Default settings are for external clients
	let mut export_period = Duration::from_secs(60);
	let mut export_timeout = Duration::from_secs(65);
	let mut detailed_metrics = false;

	if origin != Origin::External {
		export_period = Duration::from_secs(10);
		export_timeout = Duration::from_secs(15);
		detailed_metrics = true
	}
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
		.with_period(export_period) // Configures the intervening time between exports
		.with_timeout(export_timeout) // Configures the time a OT waits for an export to complete before canceling it.
		.build()?;

	global::set_meter_provider(provider);
	let meter = global::meter("avail_light_client");
	// Initialize counters - they need to persist unlike Gauges that are recreated on every record
	let counters = MetricCounter::init_counters(meter.clone(), detailed_metrics);
	Ok(Metrics {
		meter,
		attributes,
		counters,
		detailed_metrics,
	})
}
