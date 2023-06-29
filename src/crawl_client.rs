use crate::{
	network::Client,
	telemetry::{MetricValue, Metrics},
	types::{self, Delay},
};
use avail_subxt::primitives::Header;
use kate_recovery::matrix::Partition;
use std::{
	sync::Arc,
	time::{Instant, SystemTime},
};
use tokio::sync::broadcast;
use tracing::{error, info};

const ENTIRE_BLOCK: Partition = Partition {
	number: 1,
	fraction: 1,
};

pub async fn run(
	mut message_rx: broadcast::Receiver<(Header, Instant)>,
	network_client: Client,
	delay: Delay,
	metrics: Arc<impl Metrics>,
) {
	info!("Starting crawl client...");

	while let Ok((header, received_at)) = message_rx.recv().await {
		if let Some(seconds) = delay.sleep_duration(received_at) {
			info!("Sleeping for {seconds:?} seconds");
			tokio::time::sleep(seconds).await;
		}
		let block_number = header.number;
		info!("Crawling block {block_number}...");

		let start = SystemTime::now();

		let Ok(block) = types::BlockVerified::try_from((header, None)) else {
			error!("Header is not valid");
			continue;
		};

		let positions = block
			.dimensions
			.iter_extended_partition_positions(&ENTIRE_BLOCK)
			.collect::<Vec<_>>();

		let total = positions.len();
		let (fetched, _) = network_client
			.fetch_cells_from_dht(block_number, &positions)
			.await;

		let elapsed = start
			.elapsed()
			.map(|elapsed| format!("{elapsed:?}"))
			.unwrap_or("unknown".to_string());

		let success_rate = fetched.len() as f64 / total as f64;
		info!(
		    "Block {block_number}, fetched {fetched} of {total}, success rate: {success_rate}, elapsed: {elapsed}",
		    fetched = fetched.len(),
		);

		let _ = metrics.record(MetricValue::CrawlCellsSuccessRate(success_rate));
	}
}
