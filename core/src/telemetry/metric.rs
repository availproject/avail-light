use crate::types::Origin;

pub trait Value: Send + Clone {
	fn is_allowed(&self, origin: &Origin) -> bool;
}

#[cfg(test)]
pub mod tests {
	use crate::telemetry::{metric, MetricCounter, Metrics, Record};
	use async_trait::async_trait;
	use color_eyre::eyre;
	use libp2p::{kad::Mode, Multiaddr};

	pub struct MockMetrics {}

	#[async_trait]
	impl Metrics for MockMetrics {
		async fn count(&self, _: MetricCounter) {}
		async fn record<T>(&self, _: T)
		where
			T: metric::Value + Into<Record> + Send,
		{
		}
		async fn flush(&self) -> eyre::Result<()> {
			Ok(())
		}
		async fn update_operating_mode(&self, _: Mode) {}
		async fn update_multiaddress(&self, _: Multiaddr) {}
	}
}
