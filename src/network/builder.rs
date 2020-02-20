use super::Network;
use core::{future::Future, pin::Pin};

pub struct NetworkBuilder {
    /// How to spawn background tasks. If you pass `None`, then a threads pool will be used by
    /// default.
    executor: Option<Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>>,
}

/// Creates a new prototype of the network.
pub fn builder() -> NetworkBuilder {
    NetworkBuilder { executor: None }
}

impl NetworkBuilder {
    pub fn with_executor(
        mut self,
        executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
    ) -> Self {
        self.executor = Some(executor);
        self
    }

    pub fn build(self) -> Network {
        Network {}
    }
}
