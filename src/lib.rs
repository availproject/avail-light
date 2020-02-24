extern crate alloc;

pub mod chain_spec;
pub mod executor;
pub mod network;
pub mod service;
pub mod storage;
pub mod telemetry;

pub fn storage_from_chain_specs(specs: &chain_spec::ChainSpec) -> storage::Storage {
    storage::Storage::default()
}
