//! Collection of RPC servers.

use core::{fmt, pin::Pin};
use futures::prelude::*;
use hashbrown::HashMap;
use std::{io, net::SocketAddr};

#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Config<TFId, TSubId> {
    /// List of functions that the server supports that a client can call.
    pub functions: Vec<ConfigFunction<TFId>>,
    /// List of subscription functions that the server supports that a client can subscribe to.
    pub subscriptions: Vec<ConfigSubscription<TSubId>>,
}

#[derive(Debug)]
pub struct ConfigFunction<TFId> {
    /// Name of the function.
    pub name: String,
    /// Opaque identifier for this function.
    pub id: TFId,
}

#[derive(Debug)]
pub struct ConfigSubscription<TSubId> {
    /// Name of the method that starts the subscription.
    pub subscribe: String,
    /// Name of the method that ends the subscription.
    pub unsubscribe: String,
    /// Opaque identifier for this subscription.
    pub id: TSubId,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(u64);

/// Active RPC servers and their management.
pub struct RpcServers<TFId, TSubId> {
    servers: Vec<jsonrpsee::Server>,
    tasks:
        stream::SelectAll<Pin<Box<dyn Stream<Item = (jsonrpsee::server::IncomingRequest, usize)>>>>,
    /// Configuration passed at initialization.
    config: Config<TFId, TSubId>,
    /// [`RequestId`] to assign to the next incoming request.
    next_request_id: RequestId,
    /// List of requests waiting to be answered.
    pending_requests: HashMap<RequestId, (jsonrpsee::server::IncomingRequest, usize)>,
}

impl<TFId, TSubId> RpcServers<TFId, TSubId> {
    /// Creates a new empty collection.
    pub fn new(config: Config<TFId, TSubId>) -> Self {
        RpcServers {
            servers: Vec::new(),
            tasks: stream::SelectAll::new(),
            config,
            next_request_id: RequestId(0),
            pending_requests: Default::default(),
        }
    }

    /// Spawns a new HTTP JSON-RPC server.
    pub async fn spawn_http(&mut self, _addr: SocketAddr) -> Result<(), io::Error> {
        todo!()
    }

    /// Spawns a new WebSocket JSON-RPC server.
    pub async fn spawn_ws(&mut self, addr: SocketAddr) -> Result<(), io::Error> {
        let transport = jsonrpsee::transport::ws::WsTransportServer::builder(addr.into())
            .build()
            .await?;
        let server = jsonrpsee::Server::from(jsonrpsee::raw::RawServer::new(transport));
        apply_config(&self.config, &server, &mut self.tasks);
        self.servers.push(server);
        Ok(())
    }

    /// Returns the next event that happened on one of the servers.
    pub async fn next_event<'a>(&'a mut self) -> Event<'a, TFId, TSubId> {
        // It is possible for no task to be active. The only way tasks could be added would be for
        // the user to cancel the future returned by `next_event` in order to call another method.
        // For this reason, we can return an "infinite loop".
        if self.tasks.is_empty() {
            loop {
                future::pending::<()>().await;
            }
        }

        let (jsonrpsee_request, function_index) = self.tasks.next().await.unwrap();
        let local_id = self.next_request_id.clone();
        self.next_request_id.0 = self.next_request_id.0.checked_add(1).unwrap();
        let _was_in = self
            .pending_requests
            .insert(local_id, (jsonrpsee_request, function_index));
        debug_assert!(_was_in.is_none());

        Event::IncomingRequest(self.request_by_id(local_id).unwrap())
    }

    /// Returns the request with the given identifier.
    pub fn request_by_id(&mut self, local_id: RequestId) -> Option<IncomingRequest<TFId, TSubId>> {
        let function_index = self.pending_requests.get(&local_id)?.1.clone();

        Some(IncomingRequest {
            parent: self,
            local_id,
            function_index,
        })
    }
}

impl<TFId, TSubId> fmt::Debug for RpcServers<TFId, TSubId> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: print list of listened IP addresses
        f.debug_struct("RpcServers")
            .field(
                "pending_requests",
                &self.pending_requests.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[derive(Debug)]
pub enum Event<'a, TFId, TSubId> {
    IncomingRequest(IncomingRequest<'a, TFId, TSubId>),
    RequestedCancelled(RequestId),
    NewSubscription {
        local_id: RequestId,
        subscription_id: &'a mut TSubId,
    },
    SubscriptionClosed(RequestId),
}

/// A request from a connected node.
#[derive(Debug)]
pub struct IncomingRequest<'a, TFId, TSubId> {
    parent: &'a mut RpcServers<TFId, TSubId>,
    local_id: RequestId,
    function_index: usize,
}

impl<'a, TFId, TSubId> IncomingRequest<'a, TFId, TSubId> {
    /// Returns the identifier of this request, for later processing.
    pub fn id(&self) -> RequestId {
        self.local_id
    }

    /// Returns the identifier of the JSONRPC method that has been called.
    pub fn function_id(&mut self) -> &mut TFId {
        &mut self.parent.config.functions[self.function_index].id
    }

    /// Sends the given value as the answer to the request.
    pub async fn respond(
        self,
        value: Result<jsonrpsee::common::JsonValue, jsonrpsee::common::Error>,
    ) {
        let (rq, _) = self.parent.pending_requests.remove(&self.local_id).unwrap();
        rq.respond(value).await;
    }
}

/// Internal method. Applies the [`Config`] to a `jsonrpsee` server.
fn apply_config<TFId, TSubId>(
    config: &Config<TFId, TSubId>,
    server: &jsonrpsee::Server,
    executor: &mut stream::SelectAll<
        Pin<Box<dyn Stream<Item = (jsonrpsee::server::IncomingRequest, usize)>>>,
    >,
) {
    for (index, method) in config.functions.iter().enumerate() {
        let registration = match server.register_method(method.name.clone()) {
            Ok(r) => r,
            // Errors happen in case of duplicate.
            Err(_) => continue,
        };

        executor.push(Box::pin(stream::unfold(
            registration,
            move |mut registration| async move {
                let next = (registration.next().await, index);
                Some((next, registration))
            },
        )));
    }

    for (index, sub) in config.subscriptions.iter().enumerate() {
        let registration =
            match server.register_subscription(sub.subscribe.clone(), sub.unsubscribe.clone()) {
                Ok(r) => r,
                // Errors happen in case of duplicate.
                Err(_) => continue,
            };

        // TODO: ???
        /*executor.push(Box::pin(stream::unfold(
            registration,
            move |mut registration| async move {
                let next = (registration.next().await, index);
                Some((next, registration))
            },
        )));*/
    }
}
