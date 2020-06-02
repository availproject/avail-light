// TODO: fix all the compilation errors caused by the copy-pasted code
//#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod block;
pub mod chain_spec;
pub mod database;
pub mod executor;
pub mod informant;
pub mod keystore;
pub mod network;
pub mod service;
pub mod storage;

pub fn storage_from_genesis_block(specs: &chain_spec::ChainSpec) -> storage::Storage {
    let mut block0 = storage::BlockStorage::empty();
    for (key, value) in specs.genesis_top() {
        block0.insert(&key.0, &value.0);
    }

    for key in specs.genesis_top().keys() {
        if let Ok(key) = std::str::from_utf8(&key.0) {
            println!("key: {:?}", key);
        }
    }

    let mut storage = storage::Storage::empty();
    storage
        .block(
            // TODO: properly calculate this
            &"e40fbca707deed85dd9075522047d3b729aa261cc5775642b0ba43702d75ed39"
                .parse()
                .unwrap(),
        )
        .set_storage(block0)
        .unwrap();
    storage
}
