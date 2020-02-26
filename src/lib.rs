extern crate alloc;

pub mod chain_spec;
pub mod executor;
pub mod network;
pub mod service;
pub mod storage;
pub mod telemetry;

pub fn storage_from_chain_specs(specs: &chain_spec::ChainSpec) -> storage::Storage {
    let mut block0 = storage::BlockStorage::empty();
    for (key, value) in specs.genesis_top() {
        block0.insert(&key.0, &value.0);
    }
    println!("{:?}", specs.genesis_top().keys().map(|k| k.0.len()).collect::<Vec<_>>());

    let mut storage = storage::Storage::empty();
    storage.block(&"0000000000000000000000000000000000000000000000000000000000000000".parse().unwrap()).set_storage(block0).unwrap();
    storage
}
