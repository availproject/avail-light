use anyhow::{Error, Ok, Result};
use async_trait::async_trait;
use opentelemetry_api::{
	global,
	metrics::{Counter, Meter},
	KeyValue,
};
use opentelemetry_otlp::{ExportConfig, Protocol, WithExportConfig};
use std::time::Duration;
use tokio::sync::RwLock;

const ATTRIBUTE_NUMBER: usize = if cfg!(feature = "crawl") { 8 } else { 7 };

#[derive(Debug)]
pub struct Metrics {
	meter: Meter,
	session_block_counter: Counter<u64>,
	peer_id: String,
	multiaddress: RwLock<String>,
	ip: RwLock<String>,
	role: String,
	origin: String,
	avail_address: String,
	#[cfg(feature = "crawl")]
	crawl_block_delay: u64,
}

impl Metrics {
	async fn attributes(&self) -> [KeyValue; ATTRIBUTE_NUMBER] {
		[
			KeyValue::new("version", clap::crate_version!()),
			KeyValue::new("role", self.role.clone()),
			KeyValue::new("origin", self.origin.clone()),
			KeyValue::new("peerID", self.peer_id.clone()),
			KeyValue::new("multiaddress", self.multiaddress.read().await.clone()),
			KeyValue::new("ip", self.ip.read().await.clone()),
			KeyValue::new("avail_address", self.avail_address.clone()),
			#[cfg(feature = "crawl")]
			KeyValue::new("crawl_block_delay", self.crawl_block_delay.to_string()),
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
		let mut m = self.multiaddress.write().await;
		*m = multiaddr;
	}

	async fn set_ip(&self, ip: String) {
		let mut i = self.ip.write().await;
		*i = ip;
	}
}

#[async_trait]
impl super::Metrics for Metrics {
	async fn count(&self, counter: super::MetricCounter) {
		match counter {
			super::MetricCounter::SessionBlock => {
				self.session_block_counter.add(1, &self.attributes().await);
			},
		}
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
			super::MetricValue::NodeRPCFetched(number) => {
				self.record_f64("node_rpc_fetched", number).await?;
			},
			super::MetricValue::BlockConfidence(number) => {
				self.record_f64("block_confidence", number).await?;
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
			super::MetricValue::DHTPutRowsDuration(number) => {
				self.record_f64("dht_put_rows_duration", number).await?;
			},
			super::MetricValue::DHTPutRowsSuccess(number) => {
				self.record_f64("dht_put_rows_success", number).await?;
			},
			super::MetricValue::KadRoutingPeerNum(number) => {
				self.record_u64("kad_routing_table_peer_num", number as u64)
					.await?;
			},
			super::MetricValue::HealthCheck() => {
				self.record_u64("up", 1).await?;
			},
			super::MetricValue::BlockProcessingDelay(number) => {
				self.record_f64("block_processing_delay", number).await?;
			},
			#[cfg(feature = "crawl")]
			super::MetricValue::CrawlCellsSuccessRate(number) => {
				self.record_f64("crawl_cells_success_rate", number).await?;
			},
			#[cfg(feature = "crawl")]
			super::MetricValue::CrawlRowsSuccessRate(number) => {
				self.record_f64("crawl_rows_success_rate", number).await?;
			},
		};
		Ok(())
	}

	async fn set_multiaddress(&self, multiaddr: String) {
		self.set_multiaddress(multiaddr).await;
	}

	async fn set_ip(&self, ip: String) {
		self.set_ip(ip).await;
	}
}

pub fn initialize(
	endpoint: String,
	peer_id: String,
	role: String,
	origin: String,
	avail_address: String,
	#[cfg(feature = "crawl")] crawl_block_delay: u64,
) -> Result<Metrics, Error> {
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
		origin,
		avail_address,
		#[cfg(feature = "crawl")]
		crawl_block_delay,
	})
}
