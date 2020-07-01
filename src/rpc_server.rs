//! RPC servers.

// TODO: write docs

use crate::{executor, service};
use core::fmt;
use std::{io, net::SocketAddr};

pub use raw::RequestId;

/*
list of methods (temporary, for reference)

    account_nextIndex,
    author_hasKey,
    author_hasSessionKeys,
    author_insertKey,
    author_pendingExtrinsics,
    author_removeExtrinsic,
    author_rotateKeys,
    author_submitAndWatchExtrinsic,
    author_submitExtrinsic,
    author_unwatchExtrinsic,
    babe_epochAuthorship,
    chain_getBlock,
    chain_getBlockHash,
    chain_getFinalisedHead,
    chain_getFinalizedHead,
    chain_getHead,
    chain_getHeader,
    chain_getRuntimeVersion,
    chain_subscribeAllHeads,
    chain_subscribeFinalisedHeads,
    chain_subscribeFinalizedHeads,
    chain_subscribeNewHead,
    chain_subscribeNewHeads,
    chain_subscribeRuntimeVersion,
    chain_unsubscribeAllHeads,
    chain_unsubscribeFinalisedHeads,
    chain_unsubscribeFinalizedHeads,
    chain_unsubscribeNewHead,
    chain_unsubscribeNewHeads,
    chain_unsubscribeRuntimeVersion,
    childstate_getKeys,
    childstate_getStorage,
    childstate_getStorageHash,
    childstate_getStorageSize,
    grandpa_roundState,
    offchain_localStorageGet,
    offchain_localStorageSet,
    payment_queryInfo,
    state_call,
    state_callAt,
    state_getKeys,
    state_getKeysPaged,
    state_getKeysPagedAt,
    state_getMetadata,
    state_getPairs,
    state_getReadProof,
    state_getRuntimeVersion,
    state_getStorage,
    state_getStorageAt,
    state_getStorageHash,
    state_getStorageHashAt,
    state_getStorageSize,
    state_getStorageSizeAt,
    state_queryStorage,
    state_queryStorageAt,
    state_subscribeRuntimeVersion,
    state_subscribeStorage,
    state_unsubscribeRuntimeVersion,
    state_unsubscribeStorage,
    subscribe_newHead,
    system_accountNextIndex,
    system_addReservedPeer,
    system_chain,
    system_chainType,
    system_dryRun,
    system_dryRunAt,
    system_health,
    system_localListenAddresses,
    system_localPeerId,
    system_name,
    system_networkState,
    system_nodeRoles,
    system_peers,
    system_properties,
    system_removeReservedPeer,
    system_version,
    unsubscribe_newHead
*/

mod methods;
mod raw;

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
    inner: raw::RpcServers<methods::Method, ()>,
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
            subscriptions: vec![raw::ConfigSubscription {
                subscribe: "state_subscribeRuntimeVersion".into(),
                unsubscribe: "state_unsubscribeRuntimeVersion".into(),
                id: (),
            }],
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

    // TODO: this is an example example of how subscriptions would be handled
    /*pub fn notify_new_chain_head(&mut self, hash: [u8; 32]) {
        ...
    }*/

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
    inner: raw::IncomingRequest<'a, methods::Method, ()>,
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

            methods::Method::rpc_methods => {
                if let Err(err) = self.inner.expect_no_params() {
                    self.inner.respond(Err(err)).await;
                    return;
                }

                let methods: Vec<_> = methods::Method::list()
                    .map(|m| raw::JsonValue::String(m.name().to_owned()))
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
    let wasm_blob = match executor::WasmBlob::from_bytes(wasm_blob) {
        Ok(w) => w,
        Err(_) => {
            return Err(());
        }
    };

    let mut inner_vm =
        match executor::WasmVm::new(&wasm_blob, executor::FunctionToCall::MetadataMetadata) {
            Ok(v) => v,
            Err(_) => {
                return Err(());
            }
        };

    loop {
        match inner_vm.state() {
            executor::State::ReadyToRun(r) => r.run(),
            executor::State::Finished(executor::Success::MetadataMetadata(version)) => {
                break Ok(version.clone());
            }
            executor::State::Finished(_) => unreachable!(),
            executor::State::Trapped => break Err(()),

            // Since there are potential ambiguities we don't allow any storage access
            // or anything similar. The last thing we want is to have an infinite
            // recursion of runtime calls.
            _ => break Err(()),
        }
    }
}

/// Obtains the `RuntimeVersion` struct corresponding to the given Wasm runtime blob.
fn runtime_version(wasm_blob: &[u8]) -> Result<executor::CoreVersionSuccess, ()> {
    // TODO: is there maybe a better way to handle that?
    let wasm_blob = match executor::WasmBlob::from_bytes(wasm_blob) {
        Ok(w) => w,
        Err(_) => {
            return Err(());
        }
    };

    let mut inner_vm =
        match executor::WasmVm::new(&wasm_blob, executor::FunctionToCall::CoreVersion) {
            Ok(v) => v,
            Err(_) => {
                return Err(());
            }
        };

    loop {
        match inner_vm.state() {
            executor::State::ReadyToRun(r) => r.run(),
            executor::State::Finished(executor::Success::CoreVersion(version)) => {
                break Ok(version.clone());
            }
            executor::State::Finished(_) => unreachable!(),
            executor::State::Trapped => break Err(()),

            // Since there are potential ambiguities we don't allow any storage access
            // or anything similar. The last thing we want is to have an infinite
            // recursion of runtime calls.
            _ => break Err(()),
        }
    }
}
