extern crate futures;
extern crate num_cpus;
extern crate rocksdb;

use futures::stream::{self, StreamExt};
use rocksdb::{DBWithThreadMode, SingleThreaded};
use std::sync::Arc;
use std::time::SystemTime;

pub async fn sync_block_headers(
    url: String,
    start_block: u64,
    end_block: u64,
    header_store: Arc<DBWithThreadMode<SingleThreaded>>,
) {
    let fut = stream::iter(
        (start_block..(end_block + 1))
            .map(move |block_num| block_num)
            .zip((0..(end_block - start_block + 1)).map(move |_| url.clone()))
            .zip((0..(end_block - start_block + 1)).map(move |_| header_store.clone())),
    )
    .for_each_concurrent(
        num_cpus::get() << 2, // square of the number of logical CPUs available on machine
        // run those many concurrent syncing lightweight tasks, not threads
        |((block_num, url), store)| async move {
            let begin = SystemTime::now();

            match store.get_pinned_cf(
                store.cf_handle(crate::consts::BLOCK_HEADER_CF).unwrap(),
                block_num.to_be_bytes(),
            ) {
                Ok(v) => match v {
                    Some(_) => {
                        return;
                    }
                    None => {}
                },
                Err(_) => {}
            };
            // if block header look up fails, only then comes here for
            // fetching and storing block header as part of (light weight)
            // syncing process

            match super::rpc::get_block_by_number(&url, block_num).await {
                Ok(block_body) => {
                    store
                        .put_cf(
                            store.cf_handle(crate::consts::BLOCK_HEADER_CF).unwrap(),
                            block_num.to_be_bytes(),
                            serde_json::to_string(&block_body.header)
                                .unwrap()
                                .as_bytes(),
                        )
                        .unwrap();

                    println!(
                        "Synced block header of {}\t{:?}",
                        block_num,
                        begin.elapsed().unwrap()
                    );
                }
                Err(msg) => {
                    println!("error: {}", msg);
                }
            };
        },
    );
    fut.await;
}
