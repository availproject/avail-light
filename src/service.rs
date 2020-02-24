use crate::{executor, network, telemetry};
use futures::{executor::ThreadPool, prelude::*};

pub use builder::{builder, ServiceBuilder};

mod builder;

pub struct Service {
    wasm_vms: executor::WasmVirtualMachines<()>,
    /// Blob of WASM code of the runtime of the chain.
    wasm_runtime: executor::WasmBlob,
    network: network::Network,
    telemetry: telemetry::Telemetry,

    /// Optional threads pool that we keep alive.
    _threads_pool: Option<ThreadPool>,
}

pub enum Event {}

impl Service {
    pub async fn next_event(&mut self) {
        self.wasm_vms
            .execute((), &self.wasm_runtime, "Core_version", &[]);

        loop {
            let network_next = self.network.next_event();
            let telemetry_next = self.telemetry.next_event();
            futures::pin_mut!(network_next, telemetry_next);
            let _ = future::select(network_next, telemetry_next).await;
        }
    }
}
