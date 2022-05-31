extern crate futures;
extern crate num_cpus;
extern crate rocksdb;

use std::{sync::Arc, time::SystemTime};

use futures::stream::{self, StreamExt};
use ipfs_embed::{DefaultParams, Ipfs};
use rocksdb::DB;

use crate::{data, rpc};

pub async fn sync_block_headers(
	url: String,
	start_block: u64,
	end_block: u64,
	header_store: Arc<DB>,
	ipfs: Ipfs<DefaultParams>,
) {
	let blocks = (start_block..=end_block)
		.map(move |b| (b, url.clone(), header_store.clone(), ipfs.clone()));
	let fut = stream::iter(blocks).for_each_concurrent(
		num_cpus::get(), // number of logical CPUs available on machine
		// run those many concurrent syncing lightweight tasks, not threads
		|(block_num, url, store, ipfs)| async move {
			match store.get_pinned_cf(
				&store.cf_handle(crate::consts::BLOCK_HEADER_CF).unwrap(),
				block_num.to_be_bytes(),
			) {
				Ok(v) => match v {
					Some(_) => {
						return;
					},
					None => {},
				},
				Err(_) => {},
			};
			// if block header look up fails, only then comes here for
			// fetching and storing block header as part of (light weight)
			// syncing process
			let begin = SystemTime::now();

			match super::rpc::get_block_by_number(&url, block_num).await {
				Ok(block_body) => {
					log::info!(
						"Block {} app index {:?}",
						block_num,
						block_body.header.app_data_lookup.index
					);
					store
						.put_cf(
							&store.cf_handle(crate::consts::BLOCK_HEADER_CF).unwrap(),
							block_num.to_be_bytes(),
							serde_json::to_string(&block_body.header)
								.unwrap()
								.as_bytes(),
						)
						.unwrap();

					log::info!(
						"Synced block header of {}\t{:?}",
						block_num,
						begin.elapsed().unwrap()
					);

					// If it's found that this certain block is not verified
					// then it'll be verified now
					match store.get_pinned_cf(
						&store
							.cf_handle(crate::consts::CONFIDENCE_FACTOR_CF)
							.unwrap(),
						block_num.to_be_bytes(),
					) {
						Ok(v) => match v {
							Some(_) => {
								return;
							},
							None => {},
						},
						Err(_) => {},
					};

					let begin = SystemTime::now();

					// TODO: Setting max rows * 2 to match extended matrix dimensions
					let max_rows = block_body.header.extrinsics_root.rows * 2;
					let max_cols = block_body.header.extrinsics_root.cols;
					let commitment = block_body.header.extrinsics_root.commitment;
					let block_num = block_body.header.number.clone();
					// now this is in `u64`
					let mut cells = rpc::generate_random_cells(max_rows, max_cols, block_num);

					data::ipfs_priority_get_cells(&mut cells, &ipfs, &url, block_num).await;

					log::info!(
						"Fetched {} cells of block {} for verification",
						cells.len(),
						block_num
					);

					let count = crate::proof::verify_proof(
						block_num, max_rows, max_cols, cells, commitment,
					);
					log::info!(
						"Completed {} verification rounds for block {}\t{:?}",
						count,
						block_num,
						begin.elapsed().unwrap()
					);
					// write confidence factor into on-disk database
					store
						.put_cf(
							&store
								.cf_handle(crate::consts::CONFIDENCE_FACTOR_CF)
								.unwrap(),
							block_num.to_be_bytes(),
							count.to_be_bytes(),
						)
						.unwrap();
				},
				Err(msg) => {
					log::info!("error: {}", msg);
				},
			};
		},
	);
	fut.await;
}
