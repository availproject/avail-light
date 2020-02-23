use super::Service;
use crate::{executor, network, telemetry};

use core::{future::Future, pin::Pin};
use libp2p::Multiaddr;

/// Prototype for a service.
pub struct ServiceBuilder {
    /// Runtime of the Substrate chain.
    wasm_runtime: Option<executor::WasmBlob>,

    /// How to spawn background tasks. If you pass `None`, then a threads pool will be used by
    /// default.
    tasks_executor: Option<Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>>,

    /// Prototype for the network.
    network: network::NetworkBuilder,

    /// Where the telemetry should connect to.
    telemetry_endpoints: Vec<(Multiaddr, u8)>,
}

/// Creates a new prototype of the service.
pub fn builder() -> ServiceBuilder {
    ServiceBuilder {
        wasm_runtime: None, // TODO: this default is meh
        tasks_executor: None,
        network: network::builder(),
        //.with_executor(),     // TODO: centralize threading in service
        telemetry_endpoints: vec![(
            telemetry::url_to_multiaddr("wss://telemetry.polkadot.io/submit/").unwrap(),
            0,
        )],
    }
}

impl ServiceBuilder {
    /// Sets the WASM runtime blob to use.
    pub fn with_wasm_runtime(mut self, wasm_runtime: executor::WasmBlob) -> Self {
        self.wasm_runtime = Some(wasm_runtime);
        self
    }

    /// Sets how the service should spawn background tasks.
    pub fn with_tasks_executor(
        mut self,
        executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
    ) -> Self {
        self.tasks_executor = Some(executor);
        self
    }

    /// Builds the actual service, starting everything.
    pub fn build(self) -> Service {
        Service {
            network: self.network.build(),
            telemetry: telemetry::Telemetry::new(self.telemetry_endpoints, None).unwrap(),
        }
    }
}
