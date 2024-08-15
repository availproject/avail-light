use crate::{
	network::{
		p2p::Client,
		rpc::{self, Event},
	},
	telemetry::{metric, otlp::Record, MetricName, Metrics},
	types::{self, BlockVerified, Delay, Origin},
};
use kate_recovery::matrix::Partition;
use serde::{Deserialize, Serialize};
use std::{
	sync::Arc,
	time::{Duration, Instant},
};
use tokio::sync::broadcast;
use tracing::{error, info};

#[derive(Clone)]
enum CrawlMetricValue {
	CellsSuccessRate(f64),
	RowsSuccessRate(f64),
	BlockDelay(f64),
}

impl MetricName for CrawlMetricValue {
	fn name(&self) -> &'static str {
		use CrawlMetricValue::*;
		match self {
			CellsSuccessRate(_) => "avail.light.crawl.cells_success_rate",
			RowsSuccessRate(_) => "avail.light.crawl.rows_success_rate",
			BlockDelay(_) => "avail.light.crawl.block_delay",
		}
	}
}
impl From<CrawlMetricValue> for Record {
	fn from(value: CrawlMetricValue) -> Self {
		use CrawlMetricValue::*;
		use Record::*;

		let name = value.name();

		match value {
			CellsSuccessRate(number) => AvgF64(name, number),
			RowsSuccessRate(number) => AvgF64(name, number),
			BlockDelay(number) => AvgF64(name, number),
		}
	}
}

#[derive(Serialize, Deserialize, PartialEq, Clone, Copy, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum CrawlMode {
	Rows,
	Cells,
	Both,
}

impl metric::Value for CrawlMetricValue {
	// Metric filter for external peers
	// Only the metrics we wish to send to OTel should be in this list
	fn is_allowed(&self, origin: &Origin) -> bool {
		matches!(origin, Origin::Internal)
	}
}

pub async fn run(
	mut message_rx: broadcast::Receiver<Event>,
	network_client: Client,
	delay: u64,
	metrics: Arc<impl Metrics>,
	mode: CrawlMode,
	partition: Partition,
	block_sender: broadcast::Sender<BlockVerified>,
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

		let Some(extension) = &block.extension else {
			info!("Skipping block without header extension");
			continue;
		};

		if let Some(seconds) = delay.sleep_duration(received_at) {
			info!("Sleeping for {seconds:?} seconds");
			let _ = metrics
				.record(CrawlMetricValue::BlockDelay(seconds.as_secs_f64()))
				.await;
			tokio::time::sleep(seconds).await;
		}
		let block_number = block.block_num;
		info!(block_number, "Crawling block...");

		let start = Instant::now();

		if matches!(mode, CrawlMode::Cells | CrawlMode::Both) {
			let positions = extension
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
				.record(CrawlMetricValue::CellsSuccessRate(success_rate))
				.await;
		}

		if matches!(mode, CrawlMode::Cells | CrawlMode::Both) {
			let dimensions = extension.dimensions;
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
				.record(CrawlMetricValue::RowsSuccessRate(success_rate))
				.await;
		}

		if let Err(error) = block_sender.send(block) {
			error!("Cannot send block verified message: {error}");
			continue;
		}

		let elapsed = start.elapsed();
		info!(block_number, "Crawling block finished in {elapsed:?}");
	}
}
