use crate::{
	network::Client,
	telemetry::{MetricValue, Metrics},
	types::{self, CrawlMode, Delay},
};
use avail_subxt::primitives::Header;
use kate_recovery::matrix::Partition;
use std::{sync::Arc, time::Instant};
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
	mode: CrawlMode,
) {
	info!("Starting crawl client...");

	while let Ok((header, received_at)) = message_rx.recv().await {
		let block = match types::BlockVerified::try_from((header, None)) {
			Ok(block) => block,
			Err(error) => {
				error!("Header is not valid: {error}");
				continue;
			},
		};

		if let Some(seconds) = delay.sleep_duration(received_at) {
			info!("Sleeping for {seconds:?} seconds");
			tokio::time::sleep(seconds).await;
		}
		let block_number = block.block_num;
		info!(block_number, "Crawling block...");

		let start = Instant::now();

		if matches!(mode, CrawlMode::Cells | CrawlMode::Both) {
			let positions = block
				.dimensions
				.iter_extended_partition_positions(&ENTIRE_BLOCK)
				.collect::<Vec<_>>();

			let total = positions.len();
			let (fetched, _) = network_client
				.fetch_cells_from_dht(block_number, &positions)
				.await;

			let success_rate = fetched.len() as f64 / total as f64;
			info!(
				block_number,
				"Fetched {fetched} cells of {total}, success rate: {success_rate}",
				fetched = fetched.len(),
			);
			let _ = metrics.record(MetricValue::CrawlCellsSuccessRate(success_rate));
		}

		if mode == CrawlMode::Rows || mode == CrawlMode::Both {
			let dimensions = block.dimensions;
			let rows: Vec<u32> = (0..dimensions.extended_rows()).step_by(2).collect();
			let total = rows.len();
			let fetched = network_client
				.fetch_rows_from_dht(block_number, dimensions, &rows)
				.await
				.iter()
				.step_by(2)
				.flatten()
				.count();

			let success_rate = fetched as f64 / total as f64;
			info!(
				block_number,
				"Fetched {fetched} rows of {total}, success rate: {success_rate}"
			);
			let _ = metrics.record(MetricValue::CrawlRowsSuccessRate(success_rate));
		}

		let elapsed = start.elapsed();
		info!(block_number, "Crawling block finished in {elapsed:?}")
	}
}
