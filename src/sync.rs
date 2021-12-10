use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub async fn sync_block_headers(
    url: String,
    start_block: u64,
    end_block: u64,
    store: Arc<Mutex<HashMap<u64, super::types::Block>>>,
) {
    for block_num in start_block..end_block {
        let url = url.clone();
        let store = store.clone();
        tokio::spawn(async move {
            match super::rpc::get_block_by_number(&url, block_num).await {
                Ok(header) => {
                    let mut handle = store.lock().unwrap();
                    handle.insert(block_num, header);
                }
                Err(msg) => {
                    println!("error: {}", msg);
                }
            };
        });
    }
}
