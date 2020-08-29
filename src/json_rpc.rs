//! RPC servers.

#![cfg(feature = "json-rpc")]
#![cfg_attr(docsrs, doc(cfg(feature = "json-rpc")))]

// TODO: write docs

use crate::{executor, service};
use core::{convert::TryFrom as _, fmt};
use std::{io, net::SocketAddr};

pub use raw::RequestId;

mod methods;
mod raw;

pub mod websocket_server;

#[derive(Debug)]
pub struct Config {
    /// Name of the chain being run. Found in the chain specs.
    /// Example: "Polkadot CC1"
    pub chain_name: String,
    /// Type of the chain being run. Found in the chain specs.
    /// Example: "live"
    pub chain_type: String,
    /// Opaque properties of the chain being run. Found in the chain specs.
    pub chain_properties: Vec<(String, ChainProperty)>,
    /// Name of this software to report to the JSON-RPC clients.
    pub client_name: String,
    /// Version of this software to report to the JSON-RPC clients.
    /// Example: "0.8.12-03067290-x86_64-linux-gnu"
    pub client_version: String,
}

#[derive(Debug)]
pub enum ChainProperty {
    String(String),
    Number(u64),
}

pub struct RpcServers {
    inner: raw::RpcServers<methods::Method, methods::Subscription>,
    /// Configuration of the RPC servers.
    config: Config,
}

impl RpcServers {
    /// Creates a new empty collection.
    pub fn new(config: Config) -> Self {
        let raw_config = raw::Config {
            functions: methods::Method::list()
                .map(|method| raw::ConfigFunction {
                    name: method.name().to_owned(),
                    id: method,
                })
                .collect(),
            subscriptions: methods::Subscription::list()
                .map(|method| raw::ConfigSubscription {
                    subscribe: method.subscribe_method().to_owned(),
                    unsubscribe: method.unsubscribe_method().to_owned(),
                    id: method,
                })
                .collect(),
        };

        RpcServers {
            inner: raw::RpcServers::new(raw_config),
            config,
        }
    }

    /// Spawns a new HTTP JSON-RPC server.
    pub async fn spawn_http(&mut self, addr: SocketAddr) -> Result<(), io::Error> {
        self.inner.spawn_http(addr).await
    }

    /// Spawns a new WebSocket JSON-RPC server.
    pub async fn spawn_ws(&mut self, addr: SocketAddr) -> Result<(), io::Error> {
        self.inner.spawn_ws(addr).await
    }

    /// Notifies the clients that the head of the chain has been changed.
    ///
    /// Must pass the hash of the new block and the list of storage keys that have been modified
    /// as a result of the chain head update.
    ///
    /// > **Note**: You can either call this function for each block, or group all the
    /// >           `modified_keys` together.
    pub fn notify_new_chain_head<'a>(
        &mut self,
        _new_head_hash: [u8; 32],
        _modified_keys: impl Iterator<Item = &'a [u8]> + Clone,
    ) {
        // TODO: notify subscribers
    }

    /// Returns the next event that happened on one of the servers.
    pub async fn next_event<'a>(&'a mut self) -> Event<'a> {
        match self.inner.next_event().await {
            raw::Event::IncomingRequest(inner) => Event::Request(IncomingRequest {
                inner,
                config: &self.config,
            }),
            raw::Event::RequestedCancelled(local_id) => todo!(),
            // TODO: we don't care about subscription events, but there are
            // annoying borrowing errors if we just do nothing
            raw::Event::NewSubscription { .. } => todo!(),
            raw::Event::SubscriptionClosed(_) => todo!(),
        }
    }

    /// Returns a pending request by its identifier.
    pub fn request_by_id(&mut self, id: RequestId) -> Option<IncomingRequest> {
        Some(IncomingRequest {
            inner: self.inner.request_by_id(id)?,
            config: &self.config,
        })
    }
}

impl fmt::Debug for RpcServers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

/// Event produced by the [`RpcServers`].
#[derive(Debug)]
pub enum Event<'a> {
    /// A request coming from a connected node.
    Request(IncomingRequest<'a>),
}

/// A request from a connected node.
#[derive(Debug)]
pub struct IncomingRequest<'a> {
    inner: raw::IncomingRequest<'a, methods::Method, methods::Subscription>,
    config: &'a Config,
}

impl<'a> IncomingRequest<'a> {
    /// Returns the identifier of this request, for later processing.
    pub fn id(&self) -> RequestId {
        self.inner.id()
    }

    /// Answers the request using the given [`service::Service`].
    pub async fn answer(mut self, service: &service::Service) {
        match self.inner.function_id() {
            methods::Method::chain_getBlockHash => {
                let block_num = match self.inner.expect_one_u64() {
                    Ok(n) => n,
                    Err(err) => {
                        self.inner.respond(Err(err)).await;
                        return;
                    }
                };

                let rep = if let Some(hash) = service.best_effort_block_hash(block_num).await {
                    Ok(raw::JsonValue::String(format!("0x{}", hex::encode(hash))))
                } else {
                    // TODO: is this the correct error?
                    Err(raw::Error::invalid_params("Unknown block"))
                };

                self.inner.respond(rep).await;
            }

            methods::Method::chain_getHeader => todo!(),

            methods::Method::rpc_methods => {
                if let Err(err) = self.inner.expect_no_params() {
                    self.inner.respond(Err(err)).await;
                    return;
                }

                let methods: Vec<_> = methods::Method::list()
                    .map(|m| m.name())
                    .chain(methods::Subscription::list().map(|m| m.subscribe_method()))
                    .chain(methods::Subscription::list().map(|m| m.unsubscribe_method()))
                    .map(|m| raw::JsonValue::String(m.to_owned()))
                    .collect();

                self.inner
                    .respond(Ok(raw::JsonValue::Object(
                        [
                            ("version".to_owned(), raw::JsonValue::Number(1u64.into())),
                            ("methods".to_owned(), raw::JsonValue::Array(methods)),
                        ]
                        .iter()
                        .cloned() // TODO: that cloned() is crappy; Rust is adding proper support for arrays at some point
                        .collect(),
                    )))
                    .await;
            }

            methods::Method::state_getKeysPaged => {
                let params = match self.inner.params() {
                    raw::Params::Array(p) => p,
                    _ => {
                        self.inner
                            .respond(Err(raw::Error::invalid_params(String::new())))
                            .await;
                        return;
                    }
                };

                let prefix = match params.get(0) {
                    Some(raw::JsonValue::String(p)) if p.starts_with("0x") => {
                        match hex::decode(&p[2..]) {
                            Ok(p) => p,
                            Err(_) => {
                                self.inner
                                    .respond(Err(raw::Error::invalid_params(String::new())))
                                    .await;
                                return;
                            }
                        }
                    }
                    _ => {
                        self.inner
                            .respond(Err(raw::Error::invalid_params(String::new())))
                            .await;
                        return;
                    }
                };

                let count = match params.get(1) {
                    Some(raw::JsonValue::Number(n)) => {
                        if let Some(n) = n.as_u64() {
                            if n > 1000 {
                                self.inner
                                    .respond(Err(raw::Error::invalid_params(String::new())))
                                    .await;
                                return;
                            }
                            n
                        } else {
                            self.inner
                                .respond(Err(raw::Error::invalid_params(String::new())))
                                .await;
                            return;
                        }
                    }
                    _ => {
                        self.inner
                            .respond(Err(raw::Error::invalid_params(String::new())))
                            .await;
                        return;
                    }
                };

                let start_key = match params.get(2) {
                    Some(raw::JsonValue::String(p)) if p.starts_with("0x") => {
                        match hex::decode(&p[2..]) {
                            Ok(sk) => Some(sk),
                            Err(_) => {
                                self.inner
                                    .respond(Err(raw::Error::invalid_params(String::new())))
                                    .await;
                                return;
                            }
                        }
                    }
                    _ => None,
                };

                let block = match params.get(3) {
                    Some(raw::JsonValue::String(p)) if p.starts_with("0x") => {
                        match hex::decode(&p[2..]) {
                            Ok(sk) => sk,
                            Err(_) => {
                                self.inner
                                    .respond(Err(raw::Error::invalid_params(String::new())))
                                    .await;
                                return;
                            }
                        }
                    }
                    _ => service.best_block_hash().to_vec(),
                };

                let mut keys = service.storage_keys(&prefix).await;
                if let Some(start_key) = start_key {
                    // `start_key` is non-inclusive.
                    keys.retain(|k| *k > start_key);
                }
                keys.truncate(usize::try_from(count).unwrap());

                self.inner
                    .respond(Ok(raw::JsonValue::Array({
                        keys.iter()
                            .map(|k| raw::JsonValue::String(format!("0x{}", hex::encode(&k))))
                            .collect()
                    })))
                    .await;
            }

            methods::Method::state_getMetadata => {
                if let Err(err) = self.inner.expect_no_params() {
                    self.inner.respond(Err(err)).await;
                    return;
                }

                let wasm_blob: Vec<u8> = match service.storage_get(b":code").await {
                    Some(w) => w,
                    None => {
                        self.inner.respond(Err(raw::Error::internal_error())).await;
                        return;
                    }
                };

                let metadata = match metadata(&wasm_blob) {
                    Ok(rv) => rv,
                    Err(()) => {
                        self.inner.respond(Err(raw::Error::internal_error())).await;
                        return;
                    }
                };

                let metadata = format!("0x{}", hex::encode(&metadata));
                self.inner
                    .respond(Ok(raw::JsonValue::String(metadata)))
                    .await;
            }

            methods::Method::state_getRuntimeVersion => {
                if let Err(err) = self.inner.expect_no_params() {
                    self.inner.respond(Err(err)).await;
                    return;
                }

                let wasm_blob: Vec<u8> = match service.storage_get(b":code").await {
                    Some(w) => w,
                    None => {
                        self.inner.respond(Err(raw::Error::internal_error())).await;
                        return;
                    }
                };

                let runtime_version = match runtime_version(&wasm_blob) {
                    Ok(rv) => rv,
                    Err(()) => {
                        self.inner.respond(Err(raw::Error::internal_error())).await;
                        return;
                    }
                };

                self.inner
                    .respond(Ok(raw::JsonValue::Object(
                        [
                            (
                                "spec_name".to_owned(),
                                raw::JsonValue::String(runtime_version.spec_name),
                            ),
                            (
                                "impl_name".to_owned(),
                                raw::JsonValue::String(runtime_version.impl_name),
                            ),
                            (
                                "authoring_version".to_owned(),
                                raw::JsonValue::Number(runtime_version.authoring_version.into()),
                            ),
                            (
                                "spec_version".to_owned(),
                                raw::JsonValue::Number(runtime_version.spec_version.into()),
                            ),
                            (
                                "impl_version".to_owned(),
                                raw::JsonValue::Number(runtime_version.impl_version.into()),
                            ),
                            // TODO: ("apis".to_owned(), runtime_version.apis),
                            (
                                "transaction_version".to_owned(),
                                raw::JsonValue::Number(runtime_version.transaction_version.into()),
                            ),
                        ]
                        .iter()
                        .cloned() // TODO: that cloned() is crappy; Rust is adding proper support for arrays at some point
                        .collect(),
                    )))
                    .await;
            }

            methods::Method::state_queryStorageAt => {
                let params = match self.inner.params() {
                    raw::Params::Array(p) => p,
                    _ => {
                        self.inner
                            .respond(Err(raw::Error::invalid_params(String::new())))
                            .await;
                        return;
                    }
                };

                let keys_encoded = match params.get(0) {
                    Some(raw::JsonValue::Array(p)) => p,
                    _ => {
                        self.inner
                            .respond(Err(raw::Error::invalid_params(String::new())))
                            .await;
                        return;
                    }
                };

                let keys = {
                    let mut keys = Vec::with_capacity(keys_encoded.len());
                    for key in keys_encoded {
                        let s = match key {
                            raw::JsonValue::String(k) if k.starts_with("0x") => {
                                match hex::decode(&k[2..]) {
                                    Ok(sk) => sk,
                                    Err(_) => {
                                        self.inner
                                            .respond(Err(raw::Error::invalid_params(String::new())))
                                            .await;
                                        return;
                                    }
                                }
                            }
                            _ => {
                                self.inner
                                    .respond(Err(raw::Error::invalid_params(String::new())))
                                    .await;
                                return;
                            }
                        };
                        keys.push(s);
                    }
                    keys
                };

                let block = match params.get(1) {
                    Some(raw::JsonValue::String(p)) if p.starts_with("0x") => {
                        match hex::decode(&p[2..]) {
                            Ok(sk) => sk,
                            Err(_) => {
                                self.inner
                                    .respond(Err(raw::Error::invalid_params(String::new())))
                                    .await;
                                return;
                            }
                        }
                    }
                    _ => service.best_block_hash().to_vec(),
                };

                let block_hash_encoded = format!("0x{}", hex::encode(&block));

                let values = {
                    let mut values = Vec::with_capacity(keys.len());
                    for key in keys {
                        // TODO: block isn't used
                        // TODO: we discard values not in storage, is that correct?
                        if let Some(value) = service.storage_get(&key).await {
                            values
                                .push(raw::JsonValue::String(format!("0x{}", hex::encode(value))));
                        }
                    }
                    values
                };

                self.inner
                    .respond(Ok(raw::JsonValue::Array(vec![raw::JsonValue::Object(
                        [
                            (
                                "block".to_owned(),
                                raw::JsonValue::String(block_hash_encoded),
                            ),
                            ("changes".to_owned(), raw::JsonValue::Array(values)),
                        ]
                        .iter()
                        .cloned() // TODO: that cloned() is crappy; Rust is adding proper support for arrays at some point
                        .collect(),
                    )])))
                    .await;
            }

            methods::Method::system_chain => {
                if let Err(err) = self.inner.expect_no_params() {
                    self.inner.respond(Err(err)).await;
                    return;
                }

                self.inner
                    .respond(Ok(raw::JsonValue::String(self.config.chain_name.clone())))
                    .await;
            }

            methods::Method::system_chainType => {
                if let Err(err) = self.inner.expect_no_params() {
                    self.inner.respond(Err(err)).await;
                    return;
                }

                self.inner
                    .respond(Ok(raw::JsonValue::String(self.config.chain_type.clone())))
                    .await;
            }

            methods::Method::system_health => {
                if let Err(err) = self.inner.expect_no_params() {
                    self.inner.respond(Err(err)).await;
                    return;
                }

                let num_peers = service.num_network_connections();

                self.inner
                    .respond(Ok(raw::JsonValue::Object(
                        [
                            ("isSyncing".to_owned(), raw::JsonValue::Bool(true)), // TODO:
                            ("peers".to_owned(), raw::JsonValue::Number(num_peers.into())),
                            ("shouldHavePeers".to_owned(), raw::JsonValue::Bool(true)),
                        ]
                        .iter()
                        .cloned() // TODO: that cloned() is crappy; Rust is adding proper support for arrays at some point
                        .collect(),
                    )))
                    .await;
            }

            methods::Method::system_properties => {
                if let Err(err) = self.inner.expect_no_params() {
                    self.inner.respond(Err(err)).await;
                    return;
                }

                let response = raw::JsonValue::Object(
                    self.config
                        .chain_properties
                        .iter()
                        .map(|(k, v)| {
                            let v = match v {
                                ChainProperty::String(s) => raw::JsonValue::String(s.clone()),
                                ChainProperty::Number(n) => raw::JsonValue::Number((*n).into()),
                            };

                            (k.clone(), v)
                        })
                        .collect(),
                );

                self.inner.respond(Ok(response)).await;
            }

            methods::Method::system_name => {
                if let Err(err) = self.inner.expect_no_params() {
                    self.inner.respond(Err(err)).await;
                    return;
                }

                self.inner
                    .respond(Ok(raw::JsonValue::String(self.config.client_name.clone())))
                    .await;
            }

            methods::Method::system_version => {
                if let Err(err) = self.inner.expect_no_params() {
                    self.inner.respond(Err(err)).await;
                    return;
                }

                self.inner
                    .respond(Ok(raw::JsonValue::String(
                        self.config.client_version.clone(),
                    )))
                    .await;
            }

            // TODO: implement everything
            m => todo!("{:?}", m),
        }
    }
}

/// Obtains the metadata generated by the given Wasm runtime blob.
fn metadata(wasm_blob: &[u8]) -> Result<Vec<u8>, ()> {
    // TODO: is there maybe a better way to handle that?
    let mut inner_vm = {
        let prototype = match executor::WasmVmPrototype::new(&wasm_blob) {
            Ok(p) => p,
            Err(_) => {
                return Err(());
            }
        };

        match prototype.run_no_param("Metadata_metadata") {
            Ok(v) => v,
            Err(_) => {
                return Err(());
            }
        }
    };

    loop {
        match inner_vm.state() {
            executor::State::ReadyToRun(r) => r.run(),
            executor::State::Finished(data) => break Ok(data.to_owned()),
            executor::State::Trapped => break Err(()),

            // Since there are potential ambiguities we don't allow any storage access
            // or anything similar. The last thing we want is to have an infinite
            // recursion of runtime calls.
            _ => break Err(()),
        }
    }
}

/// Obtains the `RuntimeVersion` struct corresponding to the given Wasm runtime blob.
fn runtime_version(wasm_blob: &[u8]) -> Result<executor::CoreVersion, ()> {
    // TODO: is there maybe a better way to handle that?
    let prototype = match executor::WasmVmPrototype::new(&wasm_blob) {
        Ok(p) => p,
        Err(_) => {
            return Err(());
        }
    };

    let (out, _) = executor::core_version(prototype).map_err(|_| ())?;
    Ok(out)
}
