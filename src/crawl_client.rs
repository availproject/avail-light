use crate::{
	network::{
		p2p::Client,
		rpc::{self, Event},
	},
	telemetry::{MetricValue, Metrics},
	types::{self, block_matrix_partition_format, Delay},
};
use kate_recovery::matrix::Partition;
use serde::{Deserialize, Serialize};
use std::{
	sync::Arc,
	time::{Duration, Instant},
};
use tokio::sync::broadcast;
use tracing::{error, info};

pub const ENTIRE_BLOCK: Partition = Partition {
	number: 1,
	fraction: 1,
};

#[derive(Serialize, Deserialize, PartialEq, Clone, Copy, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum CrawlMode {
	Rows,
	Cells,
	Both,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct CrawlConfig {
	/// Crawl block periodically to ensure availability. (default: false)
	pub crawl_block: bool,
	/// Crawl block delay. Increment to ensure large block crawling (default: 20)
	pub crawl_block_delay: u64,
	/// Crawl block mode. Available modes are "cells", "rows" and "both" (default: "cells")
	pub crawl_block_mode: CrawlMode,
	/// Fraction and number of the block matrix part to crawl (e.g. 2/20 means second 1/20 part of a matrix) (default: None)
	#[serde(with = "block_matrix_partition_format")]
	pub crawl_block_matrix_partition: Option<Partition>,
}

impl Default for CrawlConfig {
	fn default() -> Self {
		Self {
			crawl_block: false,
			crawl_block_delay: 20,
			crawl_block_mode: CrawlMode::Cells,
			crawl_block_matrix_partition: None,
		}
	}
}

pub async fn run(
	mut message_rx: broadcast::Receiver<Event>,
	network_client: Client,
	delay: u64,
	metrics: Arc<impl Metrics>,
	mode: CrawlMode,
	partition: Partition,
) {
	info!("Starting crawl client...");

	let delay = Delay(Some(Duration::from_secs(delay)));

	while let Ok(rpc::Event::HeaderUpdate {
		header,
		received_at,
	}) = message_rx.recv().await
	{
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
			let _ = metrics
				.record(MetricValue::CrawlBlockDelay(seconds.as_secs() as f64))
				.await;
		}
		let block_number = block.block_num;
		info!(block_number, "Crawling block...");

		let start = Instant::now();

		if matches!(mode, CrawlMode::Cells | CrawlMode::Both) {
			let positions = block
				.dimensions
				.iter_extended_partition_positions(&partition)
				.collect::<Vec<_>>();

			let total = positions.len();
			let fetched = network_client
				.fetch_cells_from_dht(block_number, &positions)
				.await
				.0
				.len();

			let success_rate = fetched as f64 / total as f64;
			let partition = format!("{}/{}", partition.number, partition.fraction);
			info!(
				block_number,
				partition, success_rate, total, fetched, "Fetched block cells",
			);
			let _ = metrics
				.record(MetricValue::CrawlCellsSuccessRate(success_rate))
				.await;
		}

		if matches!(mode, CrawlMode::Cells | CrawlMode::Both) {
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
				success_rate, total, fetched, "Fetched block rows"
			);
			let _ = metrics
				.record(MetricValue::CrawlRowsSuccessRate(success_rate))
				.await;
		}

		let elapsed = start.elapsed();
		info!(block_number, "Crawling block finished in {elapsed:?}")
	}
}
