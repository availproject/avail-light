use async_trait::async_trait;
use color_eyre::Result;
use opentelemetry_api::{
	global,
	metrics::{Counter, Meter},
	KeyValue,
};
use opentelemetry_otlp::{ExportConfig, Protocol, WithExportConfig};
use std::{collections::HashMap, time::Duration};
use tokio::sync::RwLock;

use super::MetricCounter;

const ATTRIBUTE_NUMBER: usize = 8;

#[derive(Debug)]
pub struct Metrics {
	meter: Meter,
	counters: HashMap<String, Counter<u64>>,
	attributes: MetricAttributes,
}

#[derive(Debug)]
pub struct MetricAttributes {
	pub role: String,
	pub peer_id: String,
	pub ip: RwLock<String>,
	pub multiaddress: RwLock<String>,
	pub origin: String,
	pub avail_address: String,
	pub operating_mode: String,
	pub partition_size: String,
}

impl Metrics {
	async fn attributes(&self) -> [KeyValue; ATTRIBUTE_NUMBER] {
		[
			KeyValue::new("version", clap::crate_version!()),
			KeyValue::new("role", self.attributes.role.clone()),
			KeyValue::new("origin", self.attributes.origin.clone()),
			KeyValue::new("peerID", self.attributes.peer_id.clone()),
			KeyValue::new(
				"multiaddress",
				self.attributes.multiaddress.read().await.clone(),
			),
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

	async fn set_multiaddress(&self, multiaddr: String) {
		let mut m = self.attributes.multiaddress.write().await;
		*m = multiaddr;
	}
}

#[async_trait]
impl super::Metrics for Metrics {
	async fn count(&self, counter: super::MetricCounter) {
		__self.counters[&counter.to_string()].add(1, &__self.attributes().await);
	}

	async fn record(&self, value: super::MetricValue) -> Result<()> {
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
		Ok(())
	}

	async fn set_multiaddress(&self, multiaddr: String) {
		self.set_multiaddress(multiaddr).await;
	}
}

pub fn initialize(endpoint: String, attributes: MetricAttributes) -> Result<Metrics> {
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
	let initialized_counters = MetricCounter::init_counters(meter.clone());
	Ok(Metrics {
		meter,
		attributes,
		counters: initialized_counters,
	})
}
