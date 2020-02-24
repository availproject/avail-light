use super::worker;
use core::{future::Future, pin::Pin};
use libp2p::{Multiaddr, PeerId};

pub struct NetworkBuilder {
    /// How to spawn background tasks. If you pass `None`, then a threads pool will be used by
    /// default.
    executor: Option<Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>>,

    /// List of known bootnodes.
    boot_nodes: Vec<(PeerId, Multiaddr)>,
}

/// Creates a new prototype of the network.
pub fn builder() -> NetworkBuilder {
    NetworkBuilder {
        executor: None,
        boot_nodes: Vec::new(),
    }
}

impl NetworkBuilder {
    pub fn with_executor(
        mut self,
        executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
    ) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Sets the list of bootstrap nodes to use.
    ///
    /// A **bootstrap node** is a node known from the network at startup.
    pub fn with_boot_nodes(mut self, list: impl Iterator<Item = (PeerId, Multiaddr)>) -> Self {
        self.boot_nodes = list.collect();
        self
    }

    /// Starts the networking.
    pub fn build(self) -> worker::Network {
        worker::Network::start(worker::Config {})
    }
}
