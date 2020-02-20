use crate::{network, telemetry};
use super::Service;
use core::{future::Future, pin::Pin};
use libp2p::Multiaddr;

/// Prototype for a service.
pub struct ServiceBuilder {
    /// How to spawn background tasks. If you pass `None`, then a threads pool will be used by
    /// default.
    executor: Option<Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>>,

    /// Prototype for the network.
    network: network::NetworkBuilder,

    /// Where the telemetry should connect to.
    telemetry_endpoints: Vec<(Multiaddr, u8)>,
}

/// Creates a new prototype of the service.
pub fn builder() -> ServiceBuilder {
    ServiceBuilder {
        executor: None,
        network: network::builder(),
            //.with_executor(),     // TODO: centralize threading in service
        telemetry_endpoints: vec![
            (telemetry::url_to_multiaddr("wss://telemetry.polkadot.io/submit/").unwrap(), 0),
        ],
    }
}

impl ServiceBuilder {
    /// Sets how the service should spawn background tasks.
    pub fn with_executor(mut self, executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>) -> Self {
        self.executor = Some(executor);
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
