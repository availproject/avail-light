extern crate futures;
extern crate num_cpus;

use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

pub async fn sync_block_headers(
    url: String,
    start_block: u64,
    end_block: u64,
    header_store: Arc<Mutex<HashMap<u64, super::types::Header>>>,
) {
    let fut = stream::iter(
        (start_block..(end_block + 1))
            .map(move |block_num| block_num)
            .zip((0..(end_block - start_block + 1)).map(move |_| url.clone()))
            .zip((0..(end_block - start_block + 1)).map(move |_| header_store.clone())),
    )
    .for_each_concurrent(
        num_cpus::get() << 2, // twice the number of logical CPUs available on machine
        // run those many concurrent syncing lightweight tasks, not threads
        |((block_num, url), store)| async move {
            let begin = SystemTime::now();

            match super::rpc::get_block_by_number(&url, block_num).await {
                Ok(block_body) => {
                    let mut handle = store.lock().unwrap();
                    handle.insert(block_num, block_body.header);

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
