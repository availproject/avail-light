extern crate futures;
extern crate num_cpus;
extern crate rocksdb;

use std::{sync::Arc, time::SystemTime};

use futures::stream::{self, StreamExt};
use rocksdb::{DBWithThreadMode, SingleThreaded};

pub async fn sync_block_headers(
	url: String,
	start_block: u64,
	end_block: u64,
	header_store: Arc<DBWithThreadMode<SingleThreaded>>,
	app_id: u32,
) {
	let fut = stream::iter(
		(start_block..(end_block + 1))
			.map(move |block_num| block_num)
			.zip((0..(end_block - start_block + 1)).map(move |_| url.clone()))
			.zip((0..(end_block - start_block + 1)).map(move |_| header_store.clone())),
	)
	.for_each_concurrent(
		num_cpus::get(), // number of logical CPUs available on machine
		// run those many concurrent syncing lightweight tasks, not threads
		|((block_num, url), store)| async move {
			match store.get_pinned_cf(
				store.cf_handle(crate::consts::BLOCK_HEADER_CF).unwrap(),
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
							store.cf_handle(crate::consts::BLOCK_HEADER_CF).unwrap(),
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
						store
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

					let max_rows = block_body.header.extrinsics_root.rows;
					let max_cols = block_body.header.extrinsics_root.cols;
					let commitment = block_body.header.extrinsics_root.commitment;

					let cells =
						match crate::rpc::get_kate_proof(&url, block_num, max_rows, max_cols, app_id)
							.await{
								Ok(cells) => cells,
								Err(e) => {
									log::info!("❗❗failed to get kate_proof {:?}",e);
								return
							},
						};

					log::info!(
						"Fetched {} cells of app {} of block {} for verification",
						cells.len(),
						app_id,
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
							store
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
