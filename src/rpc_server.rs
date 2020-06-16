//! Collection of RPC servers.

use futures::prelude::*;
use std::{io, net::SocketAddr};

#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Config<TFId, TSubId> {
    /// List of functions that the server supports that a client can call.
    pub functions: Vec<ConfigFunction<TFId>>,
    /// List of subscription functions that the server supports that a client can subscribe to.
    pub subscriptions: Vec<>,
}

pub struct ConfigFunction<TFId> {
    /// Name of the function.
    pub name: String,
    /// Opaque identifier for this function.
    pub id: TFId,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(u64);

/// Active RPC servers and their management.
pub struct RpcServers {}

impl RpcServers<TFId, TSubId> {
    /// Creates a new empty collection.
    pub fn new(config: Config<TIfId, TSubId>) -> RpcServers {
        RpcServers {}
    }

    /// Spawns a new HTTP JSON-RPC server.
    pub fn spawn_http(&mut self, addr: impl Into<SocketAddr>) -> Result<(), io::Error> {
        todo!()
    }

    /// Spawns a new WebSocket JSON-RPC server.
    pub fn spawn_ws(&mut self, addr: impl Into<SocketAddr>) -> Result<(), io::Error> {
        todo!()
    }

    /// Returns the next event that happened on one of the servers.
    pub async fn next_event(&mut self) -> Event {
        loop {
            future::pending().await;
        }
    }

    pub fn request_by_id(&mut self, local_id: RequestId) -> Option<()> {

    }
}

pub enum Event<'a, TFId, TSubId> {
    IncomingRequest {
        local_id: RequestId,
        function_id: &'a mut TFId,
    },
    RequestedCancelled(RequestId),
    NewSubscription {
        local_id: RequestId,
        subscription_id: &'a mut TSubId,
    },
    SubscriptionClosed(RequestId),
}
