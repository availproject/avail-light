use anyhow::{Error, Ok, Result};
use async_trait::async_trait;
use opentelemetry_api::{global, metrics::Meter, KeyValue};
use opentelemetry_otlp::{ExportConfig, Protocol, WithExportConfig};
use std::time::Duration;
use tokio::sync::RwLock;

pub struct Metrics {
    meter: Meter,
    peer_id: String,
    multiaddress: RwLock<String>,
    ip: RwLock<String>,
    role: String,
    origin: String,
}

impl Metrics {
    async fn attributes(&self) -> [KeyValue; 6] {
        [
            KeyValue::new("version", clap::crate_version!()),
            KeyValue::new("role", self.role.clone()),
            KeyValue::new("peerID", self.peer_id.clone()),
            KeyValue::new("multiaddress", self.multiaddress.read().await.clone()),
            KeyValue::new("ip", self.ip.read().await.clone()),
            KeyValue::new("origin", self.origin.clone()),
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
    async fn record(&self, value: super::MetricValue) -> Result<()> {
        match value {
            super::MetricValue::HealthCheck() => {
                self.record_u64("up", 1).await?;
            }
        }
        Ok(())
    }

    async fn set_multiaddress(&self, multiaddr: String) {
        self.set_multiaddress(multiaddr).await;
    }

    async fn set_ip(&self, ip: String) {
        self.set_ip(ip).await;
    }

    async fn get_multiaddress(&self) -> String {
        self.multiaddress.read().await.clone()
    }
}

pub fn initialize(
    endpoint: String,
    peer_id: String,
    role: String,
    origin: String,
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
        .with_period(Duration::from_secs(10))
        .with_timeout(Duration::from_secs(15))
        .build()?;

    global::set_meter_provider(provider);
    let meter = global::meter("avail_light_bootstrap");

    Ok(Metrics {
        meter,
        peer_id,
        multiaddress: RwLock::new("".to_string()),
        ip: RwLock::new("".to_string()),
        role,
        origin,
    })
}
