use crate::types::Origin;

pub trait Value: Send + Clone {
	fn is_allowed(&self, origin: &Origin) -> bool;
}

#[cfg(test)]
pub mod tests {
	use crate::telemetry::{metric, MetricCounter, Metrics, Record};
	use async_trait::async_trait;
	use color_eyre::{eyre, Result};
	use libp2p::{
		kad::{Mode, QueryStats, RecordKey},
		Multiaddr,
	};

	pub struct MockMetrics {}

	#[async_trait]
	impl Metrics for MockMetrics {
		fn count(&mut self, _: MetricCounter) {}
		fn record<T>(&mut self, _: T)
		where
			T: metric::Value + Into<Record> + Send,
		{
		}
		fn flush(&mut self) -> eyre::Result<()> {
			Ok(())
		}
		fn update_multiaddress(&mut self, _address: Multiaddr) {}
		fn update_operating_mode(&mut self, _mode: Mode) {}
		fn handle_new_put_record(&mut self, _block_num: u32, _records: Vec<libp2p::kad::Record>) {}
		fn handle_successful_put_record(
			&mut self,
			_record_key: RecordKey,
			_query_stats: QueryStats,
		) -> Result<()> {
			Ok(())
		}
		fn handle_failed_put_record(
			&mut self,
			_record_key: RecordKey,
			_query_stats: QueryStats,
		) -> Result<()> {
			Ok(())
		}
	}
}
