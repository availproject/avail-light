use anyhow::Result;
use async_trait::async_trait;

pub mod otlp;

pub enum MetricValue {
    KadRoutingPeerNum(usize),
    HealthCheck(),
}

#[async_trait]
pub trait Metrics {
    async fn record(&self, value: MetricValue) -> Result<()>;
    async fn set_multiaddress(&self, multiaddrs: String);
}
