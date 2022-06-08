extern crate futures;
extern crate num_cpus;
extern crate rocksdb;

use std::{sync::Arc, time::SystemTime};

use futures::stream::{self, StreamExt};
use ipfs_embed::{DefaultParams, Ipfs};
use rocksdb::DB;

use crate::{
	data::fetch_cells_from_ipfs,
	rpc,
	types::{cell_ipfs_record, cell_to_ipfs_block},
};

pub async fn sync_block_headers(
	url: String,
	start_block: u64,
	end_block: u64,
	header_store: Arc<DB>,
	ipfs: Ipfs<DefaultParams>,
) {
	log::info!("Syncing block headers from 0 to {}", end_block);
	let blocks = (start_block..=end_block)
		.map(move |b| (b, url.clone(), header_store.clone(), ipfs.clone()));
	let fut = stream::iter(blocks).for_each_concurrent(
		num_cpus::get(), // number of logical CPUs available on machine
		// run those many concurrent syncing lightweight tasks, not threads
		|(block_num, url, store, ipfs)| async move {
			if let Ok(v) = store.get_pinned_cf(
				&store.cf_handle(crate::consts::BLOCK_HEADER_CF).unwrap(),
				block_num.to_be_bytes(),
			) {
				if v.is_some() {
					return;
				}
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
					if let Ok(v) = store.get_pinned_cf(
						&store
							.cf_handle(crate::consts::CONFIDENCE_FACTOR_CF)
							.unwrap(),
						block_num.to_be_bytes(),
					) {
						if v.is_some() {
							return;
						}
					};

					let begin = SystemTime::now();

					// TODO: Setting max rows * 2 to match extended matrix dimensions
					let max_rows = block_body.header.extrinsics_root.rows * 2;
					let max_cols = block_body.header.extrinsics_root.cols;
					let commitment = block_body.header.extrinsics_root.commitment;
					let block_num = block_body.header.number;
					// now this is in `u64`
					let positions = rpc::generate_random_cells(max_rows, max_cols);

					let ipfs_fetch_result =
						fetch_cells_from_ipfs(&ipfs, block_num, &positions).await;
					if ipfs_fetch_result.is_err() {
						return;
					}
					let (ipfs_fetched, unfetched) = ipfs_fetch_result.unwrap();

					log::info!(
						"Number of cells fetched from IPFS for block {}: {}",
						block_num,
						ipfs_fetched.len()
					);

					let rpc_fetch_result = rpc::get_kate_proof(&url, block_num, unfetched).await;
					if rpc_fetch_result.is_err() {
						return;
					}
					let rpc_fetched = rpc_fetch_result.unwrap();

					log::info!(
						"Number of cells fetched from RPC for block {}: {}",
						block_num,
						rpc_fetched.len()
					);

					let mut cells = vec![];
					cells.extend(ipfs_fetched);
					cells.extend(rpc_fetched.clone());

					if positions.len() > cells.len() {
						log::error!("Failed to fetch {} cells", positions.len() - cells.len());
						return;
					}

					log::info!(
						"Fetched {} cells of block {} for verification",
						cells.len(),
						block_num
					);

					let count = crate::proof::verify_proof(
						block_num,
						max_rows,
						max_cols,
						cells.clone(),
						commitment,
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

					// Push the randomly selected cells to IPFS
					for cell in rpc_fetched {
						if let Err(error) = ipfs.insert(cell_to_ipfs_block(cell.clone())) {
							log::info!(
								"Error pushing cell to IPFS: {}. Cell reference: {}",
								error,
								cell.reference(block_num)
							);
						}
						// Add generated CID to DHT
						if let Err(error) = ipfs
							.put_record(cell_ipfs_record(&cell, block_num), ipfs_embed::Quorum::One)
							.await
						{
							log::info!(
								"Error inserting new record to DHT: {}. Cell reference: {}",
								error,
								cell.reference(block_num)
							);
						}
					}
					if let Err(error) = ipfs.flush().await {
						log::info!("Error flushing data to disk: {}", error,);
					};
				},
				Err(msg) => {
					log::info!("error: {}", msg);
				},
			};
		},
	);
	fut.await;
}
