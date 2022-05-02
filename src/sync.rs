use std::{sync::Arc, time::SystemTime};

use anyhow::Context;
use futures::stream::{self, StreamExt};
use rocksdb::{ColumnFamilyRef, DB};

use crate::{
	consts::{BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF, MAX_CONCURRENT_TASKS},
	error::{
		self, into_error,
		BlockSync::{InvalidJsonHeader, StoreWriteFailed},
		BlockSyncCF,
	},
	proof::verify_proof,
	rpc::{get_block_by_number, get_kate_proof},
};

pub async fn sync_block_headers(
	url: &str,
	start_block: u64,
	end_block: u64,
	store: Arc<DB>,
	app_id: u32,
) {
	let fetches = stream::iter(start_block..=end_block)
		.map(|block| fetch_and_store_block_header(&store, url, block, app_id))
		.buffered(MAX_CONCURRENT_TASKS)
		.collect::<Vec<_>>()
		.await;

	fetches.into_iter().for_each(|fetch| {
		if let Err(e) = fetch {
			into_error(&e);
		}
	});
}

async fn fetch_and_store_block_header(
	store: &DB,
	url: &str,
	block_num: u64,
	app_id: u32,
) -> Result<(), error::App> {
	let block_id = block_num.to_be_bytes();

	// if block header look up fails, only then comes here for
	// fetching and storing block header as part of (light weight)
	// syncing process
	if is_cf_cached(store, &header_cf(store), block_id) {
		log::info!("Block header {} is cached, ignoring sync", block_num);
		return Ok(());
	}

	let begin = SystemTime::now();
	let block_body = get_block_by_number(url, block_num)
		.await
		.with_context(|| format!("Block {} cannot be fetched", block_num))?;
	log::info!(
		"Block {} app index {:?}",
		block_num,
		block_body.header.app_data_lookup.index
	);

	// Store header
	let header = serde_json::to_string(&block_body.header)
		.map_err(|_| InvalidJsonHeader)?
		.into_bytes();
	store
		.put_cf(&header_cf(store), block_id, header)
		.map_err(|_| StoreWriteFailed(block_num, BlockSyncCF::BlockHeader))?;
	log::info!(
		"Synced block header of {}\t{:?}",
		block_num,
		begin.elapsed()?
	);

	// If it's found that this certain block is not verified
	// then it'll be verified now
	if is_cf_cached(store, &confidence_cf(store), block_id) {
		log::info!("Block {} is already verified", block_num);
		return Ok(());
	}
	let begin = SystemTime::now();

	// TODO: Setting max rows * 2 to match extended matrix dimensions
	let max_rows = block_body.header.extrinsics_root.rows * 2;
	let max_cols = block_body.header.extrinsics_root.cols;
	let commitment = block_body.header.extrinsics_root.commitment;

	let cells = get_kate_proof(url, block_num, max_rows, max_cols, app_id)
		.await
		.with_context(|| format!("Kate proof cannot be fetched for block {}", block_num))?;

	log::info!(
		"Fetched {} cells of app {} of block {} for verification",
		cells.len(),
		app_id,
		block_num
	);

	let count = verify_proof(block_num, max_rows, max_cols, cells, commitment);
	log::info!(
		"Completed {} verification rounds for block {}\t{:?}",
		count,
		block_num,
		begin.elapsed()?
	);

	// write confidence factor into on-disk database
	store
		.put_cf(&confidence_cf(store), block_id, count.to_be_bytes())
		.map_err(|_| StoreWriteFailed(block_num, BlockSyncCF::ConfidenceFactor).into())
}

#[inline]
fn is_cf_cached<'a>(store: &'a DB, cf: &ColumnFamilyRef<'a>, block: [u8; 8]) -> bool {
	store.get_pinned_cf(cf, block).ok().flatten().is_some()
}

pub fn header_cf(store: &'_ DB) -> ColumnFamilyRef<'_> {
	store
		.cf_handle(BLOCK_HEADER_CF)
		.expect("`BLOCK_HEADER_CF` is valid .qed")
}

pub fn confidence_cf(store: &'_ DB) -> ColumnFamilyRef<'_> {
	store
		.cf_handle(CONFIDENCE_FACTOR_CF)
		.expect("`CONFIDENCE_FACTOR_CF` is valid .qed")
}
