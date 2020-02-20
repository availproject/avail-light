use crate::{network, telemetry};
use futures::prelude::*;

pub use builder::{builder, ServiceBuilder};

mod builder;

pub struct Service {
    network: network::Network,
    telemetry: telemetry::Telemetry,
}

pub enum Event {}

impl Service {
    pub async fn next_event(&mut self) {
        loop {
            let network_next = self.network.next_event();
            let telemetry_next = self.telemetry.next_event();
            futures::pin_mut!(network_next, telemetry_next);
            let _ = future::select(network_next, telemetry_next).await;
        }
    }
}
