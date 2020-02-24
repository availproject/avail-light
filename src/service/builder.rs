use super::Service;
use crate::{chain_spec::ChainSpec, executor, network, telemetry};

use alloc::sync::Arc;
use core::{future::Future, pin::Pin};
use futures::executor::ThreadPool;
use libp2p::Multiaddr;

/// Prototype for a service.
pub struct ServiceBuilder {
    /// Runtime of the Substrate chain.
    wasm_runtime: Option<executor::WasmBlob>,

    /// How to spawn background tasks. If you pass `None`, then a threads pool will be used by
    /// default.
    tasks_executor: Option<Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send + Sync>>,

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
        telemetry_endpoints: vec![(
            telemetry::url_to_multiaddr("wss://telemetry.polkadot.io/submit/").unwrap(),
            0,
        )],
    }
}

impl<'a> From<&'a ChainSpec> for ServiceBuilder {
    fn from(specs: &'a ChainSpec) -> ServiceBuilder {
        let mut builder = builder();
        builder.load_chain_specs(specs);
        builder
    }
}

impl ServiceBuilder {
    /// Overwrites the current configuration with values from the given chain specs.
    pub fn load_chain_specs(&mut self, specs: &ChainSpec) {}

    /// Sets the WASM runtime blob to use.
    pub fn with_wasm_runtime(mut self, wasm_runtime: executor::WasmBlob) -> Self {
        self.wasm_runtime = Some(wasm_runtime);
        self
    }

    /// Sets how the service should spawn background tasks.
    pub fn with_tasks_executor(
        mut self,
        executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send + Sync>,
    ) -> Self {
        self.tasks_executor = Some(executor);
        self
    }

    /// Builds the actual service, starting everything.
    pub fn build(mut self) -> Service {
        let (threads_pool, tasks_executor) = match self.tasks_executor {
            Some(tasks_executor) => {
                let tasks_executor = Arc::new(tasks_executor)
                    as Arc<Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send + Sync>>;
                (None, tasks_executor)
            }
            None => {
                let threads_pool = ThreadPool::builder()
                    .name_prefix("thread-")
                    .create()
                    .unwrap(); // TODO: don't unwrap
                let tasks_executor = {
                    let threads_pool = threads_pool.clone();
                    let exec =
                        Box::new(move |task| threads_pool.spawn_obj_ok(From::from(task))) as Box<_>;
                    Arc::new(exec)
                };
                (Some(threads_pool), tasks_executor)
            }
        };

        Service {
            wasm_vms: executor::WasmVirtualMachines::with_tasks_executor({
                let tasks_executor = tasks_executor.clone();
                move |task| (*tasks_executor)(task)
            }),
            // TODO: don't unwrap; instead, misconfig error
            wasm_runtime: self.wasm_runtime.take().unwrap(),
            network: self
                .network
                .with_executor({
                    let tasks_executor = tasks_executor.clone();
                    Box::new(move |task| (*tasks_executor)(task))
                })
                .build(),
            telemetry: telemetry::Telemetry::new(self.telemetry_endpoints, None).unwrap(),
            _threads_pool: threads_pool,
        }
    }
}
