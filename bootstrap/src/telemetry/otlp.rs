use anyhow::{Error, Ok, Result};
use async_trait::async_trait;
use opentelemetry::{global, metrics::Meter, KeyValue};
use opentelemetry_otlp::{MetricExporter, Protocol, WithExportConfig};
use opentelemetry_sdk::{
	metrics::{PeriodicReader, SdkMeterProvider},
	runtime::Tokio,
	Resource,
};
use std::time::Duration;
use tokio::sync::RwLock;

pub struct Metrics {
	meter: Meter,
	peer_id: String,
	multiaddress: RwLock<String>,
	role: String,
	origin: String,
	network: String,
}

impl Metrics {
	async fn attributes(&self) -> [KeyValue; 6] {
		[
			KeyValue::new("version", clap::crate_version!()),
			KeyValue::new("role", self.role.clone()),
			KeyValue::new("peerID", self.peer_id.clone()),
			KeyValue::new("multiaddress", self.multiaddress.read().await.clone()),
			KeyValue::new("origin", self.origin.clone()),
			KeyValue::new("network", self.network.clone()),
		]
	}

	async fn record_u64(&self, name: &'static str, value: u64) -> Result<()> {
		let attributes = self.attributes().await;
		self.meter
			.u64_observable_gauge(name)
			.with_callback(move |observer| {
				observer.observe(value, &attributes);
			})
			.build();
		Ok(())
	}

	async fn set_multiaddress(&self, multiaddr: String) {
		let mut m = self.multiaddress.write().await;
		*m = multiaddr;
	}
}

#[async_trait]
impl super::Metrics for Metrics {
	async fn record(&self, value: super::MetricValue) -> Result<()> {
		match value {
			super::MetricValue::KadRoutingPeerNum(num) => {
				self.record_u64("kad_routing_peer_num", num as u64).await?;
			},
			super::MetricValue::HealthCheck() => {
				self.record_u64("up", 1).await?;
			},
		}
		Ok(())
	}

	async fn set_multiaddress(&self, multiaddr: String) {
		self.set_multiaddress(multiaddr).await;
	}
}

pub fn initialize(
	endpoint: String,
	peer_id: String,
	role: String,
	origin: String,
	network: String,
) -> Result<Metrics, Error> {
	let exporter = MetricExporter::builder()
		.with_tonic()
		.with_endpoint(&endpoint)
		.with_protocol(Protocol::Grpc)
		.with_timeout(Duration::from_secs(30))
		.build()?;

	let reader = PeriodicReader::builder(exporter, Tokio)
		.with_interval(Duration::from_secs(30))
		.with_timeout(Duration::from_secs(30))
		.build();

	let provider = SdkMeterProvider::builder()
		.with_reader(reader)
		.with_resource(Resource::new(vec![KeyValue::new(
			"service.name",
			"bootstrap".to_string(),
		)]))
		.build();

	global::set_meter_provider(provider);
	let meter = global::meter("avail_light_bootstrap");

	Ok(Metrics {
		meter,
		peer_id,
		multiaddress: RwLock::new("".to_string()),
		role,
		origin,
		network,
	})
}
