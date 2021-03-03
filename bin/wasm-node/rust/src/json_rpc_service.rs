// Smoldot
// Copyright (C) 2019-2021  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Background JSON-RPC service.
//!
//! The [`start`] function returns a future whose role is to pull events using
//! [`ffi::next_json_rpc`] and send back answers using [`ffi::emit_json_rpc_response`].
//!
//! > **Note**: Because of the racy nature of these two functions, it is strongly discouraged to
//! >           spawn multiple JSON-RPC services, especially if they don't use the same
//! >           [`sync_service::SyncService`].

// TODO: doc
// TODO: re-review this once finished

use crate::{ffi, network_service, runtime_service, sync_service, transactions_service};

use futures::{channel::oneshot, lock::Mutex, prelude::*};
use methods::MethodCall;
use smoldot::{
    chain_spec, executor, header,
    json_rpc::{self, methods},
    network::protocol,
};
use std::{
    collections::HashMap,
    convert::TryFrom as _,
    iter,
    pin::Pin,
    sync::{atomic, Arc},
};

/// Configuration for a JSON-RPC service.
pub struct Config {
    /// Closure that spawns background tasks.
    pub tasks_executor: Box<dyn FnMut(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    /// Service responsible for the networking of the chain, and index of the chain within the
    /// network service to handle.
    pub network_service: (Arc<network_service::NetworkService>, usize),

    /// Service responsible for synchronizing the chain.
    pub sync_service: Arc<sync_service::SyncService>,

    /// Service responsible for emitting transactions and tracking their state.
    pub transactions_service: Arc<transactions_service::TransactionsService>,

    /// Service that provides a ready-to-be-called runtime for the current best block.
    pub runtime_service: Arc<runtime_service::RuntimeService>,

    /// Specifications of the chain.
    pub chain_spec: chain_spec::ChainSpec,

    /// Hash of the genesis block of the chain.
    ///
    /// > **Note**: This can be derived from a [`chain_spec::ChainSpec`]. While the [`start`]
    /// >           function could in theory use the [`Config::chain_spec`] parameter to derive
    /// >           this value, doing so is quite expensive. We prefer to require this value
    /// >           from the upper layer instead, as it is most likely needed anyway.
    pub genesis_block_hash: [u8; 32],

    /// Hash of the storage trie root of the genesis block of the chain.
    ///
    /// > **Note**: This can be derived from a [`chain_spec::ChainSpec`]. While the [`start`]
    /// >           function could in theory use the [`Config::chain_spec`] parameter to derive
    /// >           this value, doing so is quite expensive. We prefer to require this value
    /// >           from the upper layer instead.
    pub genesis_block_state_root: [u8; 32],
}

/// Initializes the JSON-RPC service with the given configuration.
pub async fn start(config: Config) {
    let (_finalized_block_header, finalized_blocks_subscription) =
        config.sync_service.subscribe_best().await;
    let (best_block_header, best_blocks_subscription) = config.sync_service.subscribe_best().await;
    debug_assert_eq!(_finalized_block_header, best_block_header);
    let best_block_hash = header::hash_from_scale_encoded_header(&best_block_header);

    let mut known_blocks = lru::LruCache::new(256);
    known_blocks.put(
        best_block_hash,
        header::decode(&best_block_header).unwrap().into(),
    );

    let client = Arc::new(JsonRpcService {
        tasks_executor: Mutex::new(config.tasks_executor),
        chain_spec: config.chain_spec,
        network_service: config.network_service.0,
        network_chain_index: config.network_service.1,
        sync_service: config.sync_service,
        runtime_service: config.runtime_service,
        transactions_service: config.transactions_service,
        blocks: Mutex::new(Blocks {
            known_blocks,
            best_block: best_block_hash,
            finalized_block: best_block_hash,
        }),
        genesis_block: config.genesis_block_hash,
        next_subscription: atomic::AtomicU64::new(0),
        all_heads: Mutex::new(HashMap::new()),
        new_heads: Mutex::new(HashMap::new()),
        finalized_heads: Mutex::new(HashMap::new()),
        storage: Mutex::new(HashMap::new()),
        transactions: Mutex::new(HashMap::new()),
        runtime_specs: Mutex::new(HashMap::new()),
    });

    // Spawns a task whose role is to update `blocks` with the new best and finalized blocks.
    (client.clone().tasks_executor.lock().await)({
        let client = client.clone();
        Box::pin(async move {
            futures::pin_mut!(best_blocks_subscription, finalized_blocks_subscription);

            loop {
                match future::select(
                    best_blocks_subscription.next(),
                    finalized_blocks_subscription.next(),
                )
                .await
                {
                    future::Either::Left((Some(block), _)) => {
                        let hash = header::hash_from_scale_encoded_header(&block);
                        let mut blocks = client.blocks.lock().await;
                        let blocks = &mut *blocks;
                        blocks.best_block = hash;
                        // As a small trick, we re-query the finalized block from `known_blocks` in
                        let header = header::decode(&block).unwrap().into();
                        // order to ensure that it never leaves the LRU cache.
                        blocks.known_blocks.get(&blocks.finalized_block);
                        blocks.known_blocks.put(hash, header);
                    }
                    future::Either::Right((Some(block), _)) => {
                        let hash = header::hash_from_scale_encoded_header(&block);
                        let header = header::decode(&block).unwrap().into();
                        let mut blocks = client.blocks.lock().await;
                        blocks.finalized_block = hash;
                        blocks.known_blocks.put(hash, header);
                    }

                    // One of the two streams is over.
                    _ => break,
                }
            }
        })
    });

    // Spawn the main requests handling task.
    (client.clone().tasks_executor.lock().await)(Box::pin(async move {
        loop {
            let json_rpc_request = ffi::next_json_rpc().await;

            // Each incoming request gets its own separate task.
            let client_clone = client.clone();
            (client.tasks_executor.lock().await)(Box::pin(async move {
                let request_string = match String::from_utf8(Vec::from(json_rpc_request)) {
                    Ok(s) => s,
                    Err(error) => {
                        log::warn!(
                            target: "json-rpc",
                            "Failed to parse JSON-RPC query as UTF-8: {}", error
                        );
                        return;
                    }
                };

                log::debug!(
                    target: "json-rpc",
                    "JSON-RPC => {:?}{}",
                    if request_string.len() > 100 { &request_string[..100] } else { &request_string[..] },
                    if request_string.len() > 100 { "…" } else { "" }
                );

                let (request_id, call) = match methods::parse_json_call(&request_string) {
                    Ok(rq) => rq,
                    Err(error) => {
                        log::warn!(
                            target: "json-rpc",
                            "Ignoring malformed JSON-RPC call: {}", error
                        );
                        return;
                    }
                };

                client_clone.handle_rpc(request_id, call).await;
            }));
        }
    }));
}

struct JsonRpcService {
    /// See [`Config::tasks_executor`].
    tasks_executor: Mutex<Box<dyn FnMut(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>>,

    chain_spec: chain_spec::ChainSpec,

    /// See [`Config::network_service`].
    network_service: Arc<network_service::NetworkService>,
    /// See [`Config::network_service`].
    network_chain_index: usize,
    /// See [`Config::sync_service`].
    sync_service: Arc<sync_service::SyncService>,
    /// See [`Config::runtime_service`].
    runtime_service: Arc<runtime_service::RuntimeService>,
    /// See [`Config::transactions_service`].
    transactions_service: Arc<transactions_service::TransactionsService>,

    /// Blocks that are temporarily saved in order to serve JSON-RPC requests.
    blocks: Mutex<Blocks>,

    /// Hash of the genesis block.
    /// Keeping the genesis block is important, as the genesis block hash is included in
    /// transaction signatures, and must therefore be queried by upper-level UIs.
    genesis_block: [u8; 32],

    next_subscription: atomic::AtomicU64,

    /// For each active finalized blocks subscription (the key), a sender. If the user
    /// unsubscribes, send the unsubscription request ID of the channel in order to close the
    /// subscription.
    all_heads: Mutex<HashMap<String, oneshot::Sender<String>>>,

    /// Same principle as [`JsonRpcService::all_heads`], but for new heads subscriptions.
    new_heads: Mutex<HashMap<String, oneshot::Sender<String>>>,

    /// Same principle as [`JsonRpcService::all_heads`], but for finalized heads subscriptions.
    finalized_heads: Mutex<HashMap<String, oneshot::Sender<String>>>,

    /// Same principle as [`JsonRpcService::all_heads`], but for storage subscriptions.
    storage: Mutex<HashMap<String, oneshot::Sender<String>>>,

    /// Same principle as [`JsonRpcService::all_heads`], but for transactions.
    transactions: Mutex<HashMap<String, oneshot::Sender<String>>>,

    /// Same principle as [`JsonRpcService::all_heads`], but for runtime specs.
    runtime_specs: Mutex<HashMap<String, oneshot::Sender<String>>>,
}

struct Blocks {
    /// Blocks that are temporarily saved in order to serve JSON-RPC requests.
    ///
    /// Always contains `best_block` and `finalized_block`.
    known_blocks: lru::LruCache<[u8; 32], header::Header>,

    /// Hash of the current best block.
    best_block: [u8; 32],

    /// Hash of the latest finalized block.
    finalized_block: [u8; 32],
}

impl JsonRpcService {
    /// Send back a response or a notification to the JSON-RPC client.
    ///
    /// > **Note**: This method wraps around [`ffi::emit_json_rpc_response`] and exists primarily
    /// >           in order to print a log message.
    fn send_back(&self, message: &str) {
        log::debug!(
            target: "json-rpc",
            "JSON-RPC <= {}{}",
            if message.len() > 100 { &message[..100] } else { &message[..] },
            if message.len() > 100 { "…" } else { "" }
        );

        ffi::emit_json_rpc_response(message);
    }

    /// Analyzes the given JSON-RPC call and processes it.
    ///
    /// Depending on the request, either calls [`JsonRpcService::send_back`] immediately or
    /// spawns a background task for further processing.
    async fn handle_rpc(self: Arc<JsonRpcService>, request_id: &str, call: MethodCall) {
        // Most calls are handled directly in this method's body. The most voluminous (in terms
        // of lines of code) have their dedicated methods.
        match call {
            methods::MethodCall::author_pendingExtrinsics {} => {
                // TODO: ask transactions service
                self.send_back(
                    &methods::Response::author_pendingExtrinsics(Vec::new())
                        .to_json_response(request_id),
                );
            }
            methods::MethodCall::author_submitExtrinsic { transaction } => {
                // Send the transaction to the transactions service. It will be sent to the
                // rest of the network asynchronously.
                self.transactions_service
                    .submit_extrinsic(&transaction.0)
                    .await;

                // In Substrate, `author_submitExtrinsic` returns the hash of the extrinsic. It
                // is unclear whether it has to actually be the hash of the transaction or if it
                // could be any opaque value. Additionally, there isn't any other JSON-RPC method
                // that accepts as parameter the value returned here. When in doubt, we return
                // the hash as well.
                let mut hash_context = blake2_rfc::blake2b::Blake2b::new(32);
                hash_context.update(&transaction.0);
                let mut transaction_hash: [u8; 32] = Default::default();
                transaction_hash.copy_from_slice(hash_context.finalize().as_bytes());

                self.send_back(
                    &methods::Response::author_submitExtrinsic(methods::HashHexString(
                        transaction_hash,
                    ))
                    .to_json_response(request_id),
                );
            }
            methods::MethodCall::author_submitAndWatchExtrinsic { transaction } => {
                self.submit_and_watch_extrinsic(request_id, transaction)
                    .await
            }
            methods::MethodCall::author_unwatchExtrinsic { subscription } => {
                let invalid =
                    if let Some(cancel_tx) = self.transactions.lock().await.remove(&subscription) {
                        // `cancel_tx` might have been closed if the channel from the transactions
                        // service has been closed too. This is not an error.
                        let _ = cancel_tx.send(request_id.to_owned());
                        false
                    } else {
                        true
                    };

                if invalid {
                    self.send_back(
                        &methods::Response::author_unwatchExtrinsic(false)
                            .to_json_response(request_id),
                    );
                } else {
                }
            }
            methods::MethodCall::chain_getBlock { hash } => {
                // `hash` equal to `None` means "the current best block".
                let hash = match hash {
                    Some(h) => h.0,
                    None => self.blocks.lock().await.best_block,
                };

                // Block bodies and justifications aren't stored locally. Ask the network.
                let result = self
                    .network_service
                    .clone()
                    .block_query(
                        self.network_chain_index,
                        hash,
                        protocol::BlocksRequestFields {
                            header: true,
                            body: true,
                            justification: true,
                        },
                    )
                    .await;

                // The `block_query` function guarantees that the header and body are present and
                // are correct.

                self.send_back(&if let Ok(block) = result {
                    methods::Response::chain_getBlock(methods::Block {
                        extrinsics: block
                            .body
                            .unwrap()
                            .into_iter()
                            .map(methods::Extrinsic)
                            .collect(),
                        header: header_conv(header::decode(&block.header.unwrap()).unwrap()),
                        justification: block.justification.map(methods::HexString),
                    })
                    .to_json_response(request_id)
                } else {
                    "null".to_owned()
                });
            }
            methods::MethodCall::chain_getBlockHash { height } => {
                self.get_block_hash(request_id, height).await;
            }
            methods::MethodCall::chain_getFinalizedHead {} => {
                self.send_back(
                    &methods::Response::chain_getFinalizedHead(methods::HashHexString(
                        self.blocks.lock().await.finalized_block,
                    ))
                    .to_json_response(request_id),
                );
            }
            methods::MethodCall::chain_getHeader { hash } => {
                let hash = match hash {
                    Some(h) => h.0,
                    None => self.blocks.lock().await.best_block,
                };

                self.send_back(&match self.header_query(&hash).await {
                    Ok(header) => {
                        let decoded = header::decode(&header).unwrap();
                        methods::Response::chain_getHeader(header_conv(decoded))
                            .to_json_response(request_id)
                    }
                    // TODO: error or null?
                    Err(()) => json_rpc::parse::build_success_response(request_id, "null"),
                });
            }
            methods::MethodCall::chain_subscribeAllHeads {} => {
                self.subscribe_all_heads(request_id).await;
            }
            methods::MethodCall::chain_subscribeNewHeads {} => {
                self.subscribe_new_heads(request_id).await;
            }
            methods::MethodCall::chain_subscribeFinalizedHeads {} => {
                self.subscribe_finalized_heads(request_id).await;
            }
            methods::MethodCall::chain_unsubscribeFinalizedHeads { subscription } => {
                let invalid = if let Some(cancel_tx) =
                    self.finalized_heads.lock().await.remove(&subscription)
                {
                    cancel_tx.send(request_id.to_owned()).is_err()
                } else {
                    true
                };

                if invalid {
                    self.send_back(
                        &methods::Response::chain_unsubscribeFinalizedHeads(false)
                            .to_json_response(request_id),
                    );
                } else {
                }
            }
            methods::MethodCall::payment_queryInfo { extrinsic, hash } => {
                assert!(hash.is_none()); // TODO: handle when hash != None
                                         // TODO: complete hack
                self.send_back(
                    &methods::Response::payment_queryInfo(methods::RuntimeDispatchInfo {
                        weight: 220429000,                     // TODO: no
                        class: methods::DispatchClass::Normal, // TODO: no
                        partial_fee: 15600000001,              // TODO: no
                    })
                    .to_json_response(request_id),
                );
            }
            methods::MethodCall::rpc_methods {} => {
                self.send_back(
                    &methods::Response::rpc_methods(methods::RpcMethods {
                        version: 1,
                        methods: methods::MethodCall::method_names()
                            .map(|n| n.into())
                            .collect(),
                    })
                    .to_json_response(request_id),
                );
            }
            methods::MethodCall::state_getKeysPaged {
                prefix,
                count,
                start_key,
                hash,
            } => {
                assert!(hash.is_none()); // TODO: not implemented

                let mut lock = self.blocks.lock().await;

                let block_hash = lock.best_block;
                let state_root = lock.known_blocks.get(&block_hash).unwrap().state_root;
                drop(lock);

                let outcome = self
                    .network_service
                    .clone()
                    .storage_prefix_keys_query(
                        self.network_chain_index,
                        &block_hash,
                        &prefix.unwrap().0, // TODO: don't unwrap! what is this Option?
                        &state_root,
                    )
                    .await;

                self.send_back(&match outcome {
                    Ok(keys) => {
                        // TODO: instead of requesting all keys with that prefix from the network, pass `start_key` to the network service
                        let out = keys
                            .into_iter()
                            .filter(|k| start_key.as_ref().map_or(true, |start| k >= &start.0)) // TODO: not sure if start should be in the set or not?
                            .map(methods::HexString)
                            .take(usize::try_from(count).unwrap_or(usize::max_value()))
                            .collect::<Vec<_>>();
                        methods::Response::state_getKeysPaged(out).to_json_response(request_id)
                    }
                    Err(error) => json_rpc::parse::build_error_response(
                        request_id,
                        json_rpc::parse::ErrorResponse::ServerError(-32000, &error.to_string()),
                        None,
                    ),
                });
            }
            methods::MethodCall::state_queryStorageAt { keys, at } => {
                let blocks = self.blocks.lock().await;
                let at = at.as_ref().map(|h| h.0).unwrap_or(blocks.best_block);

                // TODO: have no idea what this describes actually
                let mut out = methods::StorageChangeSet {
                    block: methods::HashHexString(blocks.best_block),
                    changes: Vec::new(),
                };

                // Drop the lock to make sure that we don't accidentally lock it again below.
                drop(blocks);

                for key in keys {
                    // TODO: parallelism?
                    if let Ok(value) = self.storage_query(&key.0, &at).await {
                        out.changes.push((key, value.map(methods::HexString)));
                    }
                }

                self.send_back(
                    &methods::Response::state_queryStorageAt(vec![out])
                        .to_json_response(request_id),
                );
            }
            methods::MethodCall::state_getMetadata {} => {
                let response = match self.runtime_service.clone().metadata().await {
                    Ok(metadata) => {
                        methods::Response::state_getMetadata(methods::HexString(metadata))
                            .to_json_response(request_id)
                    }
                    Err(error) => json_rpc::parse::build_error_response(
                        request_id,
                        json_rpc::parse::ErrorResponse::ServerError(-32000, &error.to_string()),
                        None,
                    ),
                };

                self.send_back(&response);
            }
            methods::MethodCall::state_getStorage { key, hash } => {
                let hash = hash
                    .as_ref()
                    .map(|h| h.0)
                    .unwrap_or(self.blocks.lock().await.best_block);

                self.send_back(&match self.storage_query(&key.0, &hash).await {
                    Ok(Some(value)) => {
                        methods::Response::state_getStorage(methods::HexString(value.to_owned())) // TODO: overhead
                            .to_json_response(request_id)
                    }
                    Ok(None) => json_rpc::parse::build_success_response(request_id, "null"),
                    Err(error) => json_rpc::parse::build_error_response(
                        request_id,
                        json_rpc::parse::ErrorResponse::ServerError(-32000, &error.to_string()),
                        None,
                    ),
                });
            }
            methods::MethodCall::state_subscribeRuntimeVersion {} => {
                let subscription = self
                    .next_subscription
                    .fetch_add(1, atomic::Ordering::Relaxed)
                    .to_string();

                let (current_specs, spec_changes) =
                    self.runtime_service.subscribe_runtime_version().await;

                let (unsubscribe_tx, mut unsubscribe_rx) = oneshot::channel();
                self.runtime_specs
                    .lock()
                    .await
                    .insert(subscription.clone(), unsubscribe_tx);

                self.send_back(
                    &methods::Response::state_subscribeRuntimeVersion(&subscription)
                        .to_json_response(request_id),
                );

                let notification = if let Ok(runtime_spec) = current_specs {
                    let runtime_spec = runtime_spec.decode();
                    serde_json::to_string(&methods::RuntimeVersion {
                        spec_name: runtime_spec.spec_name.into(),
                        impl_name: runtime_spec.impl_name.into(),
                        authoring_version: u64::from(runtime_spec.authoring_version),
                        spec_version: u64::from(runtime_spec.spec_version),
                        impl_version: u64::from(runtime_spec.impl_version),
                        transaction_version: runtime_spec.transaction_version.map(u64::from),
                        apis: runtime_spec.apis,
                    })
                    .unwrap()
                } else {
                    "null".to_string()
                };

                self.send_back(&smoldot::json_rpc::parse::build_subscription_event(
                    "state_runtimeVersion",
                    &subscription,
                    &notification,
                ));

                let client = self.clone();
                (self.tasks_executor.lock().await)(Box::pin(async move {
                    futures::pin_mut!(spec_changes);

                    loop {
                        // Wait for either a new storage update, or for the subscription to be canceled.
                        let next_change = spec_changes.next();
                        futures::pin_mut!(next_change);
                        match future::select(next_change, &mut unsubscribe_rx).await {
                            future::Either::Left((new_runtime, _)) => {
                                let notification_body =
                                    if let Ok(runtime_spec) = new_runtime.unwrap() {
                                        let runtime_spec = runtime_spec.decode();
                                        serde_json::to_string(&methods::RuntimeVersion {
                                            spec_name: runtime_spec.spec_name.into(),
                                            impl_name: runtime_spec.impl_name.into(),
                                            authoring_version: u64::from(
                                                runtime_spec.authoring_version,
                                            ),
                                            spec_version: u64::from(runtime_spec.spec_version),
                                            impl_version: u64::from(runtime_spec.impl_version),
                                            transaction_version: runtime_spec
                                                .transaction_version
                                                .map(u64::from),
                                            apis: runtime_spec.apis,
                                        })
                                        .unwrap()
                                    } else {
                                        "null".to_string()
                                    };

                                client.send_back(
                                    &smoldot::json_rpc::parse::build_subscription_event(
                                        "state_runtimeVersion",
                                        &subscription,
                                        &notification_body,
                                    ),
                                );
                            }
                            future::Either::Right((Ok(unsub_request_id), _)) => {
                                let response =
                                    methods::Response::state_unsubscribeRuntimeVersion(true)
                                        .to_json_response(&unsub_request_id);
                                client.send_back(&response);
                                break;
                            }
                            future::Either::Right((Err(_), _)) => break,
                        }
                    }
                }));
            }
            methods::MethodCall::state_subscribeStorage { list } => {
                self.subscribe_storage(request_id, list).await;
            }
            methods::MethodCall::state_unsubscribeStorage { subscription } => {
                let invalid =
                    if let Some(cancel_tx) = self.storage.lock().await.remove(&subscription) {
                        cancel_tx.send(request_id.to_owned()).is_err()
                    } else {
                        true
                    };

                if invalid {
                    self.send_back(
                        &methods::Response::state_unsubscribeStorage(false)
                            .to_json_response(request_id),
                    );
                }
            }
            methods::MethodCall::state_getRuntimeVersion {} => {
                let (current_specs, _) = self.runtime_service.subscribe_runtime_version().await;
                self.send_back(&if let Ok(runtime_spec) = current_specs {
                    let runtime_spec = runtime_spec.decode();
                    methods::Response::state_getRuntimeVersion(methods::RuntimeVersion {
                        spec_name: runtime_spec.spec_name.into(),
                        impl_name: runtime_spec.impl_name.into(),
                        authoring_version: u64::from(runtime_spec.authoring_version),
                        spec_version: u64::from(runtime_spec.spec_version),
                        impl_version: u64::from(runtime_spec.impl_version),
                        transaction_version: runtime_spec.transaction_version.map(u64::from),
                        apis: runtime_spec.apis,
                    })
                    .to_json_response(request_id)
                } else {
                    json_rpc::parse::build_error_response(
                        request_id,
                        json_rpc::parse::ErrorResponse::ServerError(-32000, "Invalid runtime"),
                        None,
                    )
                });
            }
            methods::MethodCall::system_accountNextIndex { account } => {
                self.send_back(&match self
                    .runtime_service
                    .recent_best_block_runtime_call(
                        "AccountNonceApi_account_nonce",
                        iter::once(&account.0),
                    )
                    .await
                {
                    Ok(return_value) => {
                        // TODO: we get a u32 when expecting a u64; figure out problem
                        // TODO: don't unwrap
                        let index =
                            u32::from_le_bytes(<[u8; 4]>::try_from(&return_value[..]).unwrap());
                        methods::Response::system_accountNextIndex(u64::from(index))
                            .to_json_response(request_id)
                    }
                    Err(error) => json_rpc::parse::build_error_response(
                        request_id,
                        json_rpc::parse::ErrorResponse::ServerError(-32000, &error.to_string()),
                        None,
                    ),
                });
            }
            methods::MethodCall::system_chain {} => {
                self.send_back(
                    &methods::Response::system_chain(self.chain_spec.name())
                        .to_json_response(request_id),
                );
            }
            methods::MethodCall::system_chainType {} => {
                self.send_back(
                    &methods::Response::system_chainType(self.chain_spec.chain_type())
                        .to_json_response(request_id),
                );
            }
            methods::MethodCall::system_health {} => {
                self.send_back(
                    &methods::Response::system_health(methods::SystemHealth {
                        is_syncing: !self.sync_service.is_near_head_of_chain_heuristic().await,
                        peers: u64::try_from(self.network_service.peers_list().await.count())
                            .unwrap_or(u64::max_value()),
                        should_have_peers: self.chain_spec.has_live_network(),
                    })
                    .to_json_response(request_id),
                );
            }
            methods::MethodCall::system_name {} => {
                self.send_back(
                    &methods::Response::system_name(env!("CARGO_PKG_NAME"))
                        .to_json_response(request_id),
                );
            }
            methods::MethodCall::system_peers {} => {
                // TODO: return proper response
                self.send_back(
                    &methods::Response::system_peers(
                        self.network_service
                            .peers_list()
                            .await
                            .map(|peer_id| methods::SystemPeer {
                                peer_id: peer_id.to_string(),
                                roles: "unknown".to_string(),
                                best_hash: methods::HashHexString([0x0; 32]),
                                best_number: 0,
                            })
                            .collect(),
                    )
                    .to_json_response(request_id),
                );
            }
            methods::MethodCall::system_properties {} => {
                self.send_back(
                    &methods::Response::system_properties(
                        serde_json::from_str(self.chain_spec.properties()).unwrap(),
                    )
                    .to_json_response(request_id),
                );
            }
            methods::MethodCall::system_version {} => {
                self.send_back(
                    &methods::Response::system_version(env!("CARGO_PKG_VERSION"))
                        .to_json_response(request_id),
                );
            }
            _method => {
                log::error!(target: "json-rpc", "JSON-RPC call not supported yet: {:?}", _method);
                self.send_back(&json_rpc::parse::build_error_response(
                    request_id,
                    json_rpc::parse::ErrorResponse::ServerError(
                        -32000,
                        "Not implemented in smoldot yet",
                    ),
                    None,
                ));
            }
        }
    }

    /// Handles a call to [`methods::MethodCall::author_submitAndWatchExtrinsic`].
    async fn submit_and_watch_extrinsic(
        self: Arc<JsonRpcService>,
        request_id: &str,
        transaction: methods::HexString,
    ) {
        let mut transaction_updates = self
            .transactions_service
            .submit_extrinsic(&transaction.0)
            .await;

        let subscription = self
            .next_subscription
            .fetch_add(1, atomic::Ordering::Relaxed)
            .to_string();

        let (unsubscribe_tx, mut unsubscribe_rx) = oneshot::channel();
        self.transactions
            .lock()
            .await
            .insert(subscription.clone(), unsubscribe_tx);

        let confirmation = methods::Response::author_submitAndWatchExtrinsic(&subscription)
            .to_json_response(request_id);

        // Spawn a separate task for the transaction updates.
        let client = self.clone();
        (self.tasks_executor.lock().await)(Box::pin(async move {
            // Send back to the user the confirmation of the registration.
            client.send_back(&confirmation);

            loop {
                // Wait for either a status update block, or for the subscription to
                // be canceled.
                let next_update = transaction_updates.next();
                futures::pin_mut!(next_update);
                match future::select(next_update, &mut unsubscribe_rx).await {
                    future::Either::Left((Some(update), _)) => {
                        let update = match update {
                            transactions_service::TransactionStatus::Broadcast(peers) => {
                                methods::TransactionStatus::Broadcast(
                                    peers.into_iter().map(|peer| peer.to_base58()).collect(),
                                )
                            }
                            transactions_service::TransactionStatus::InBlock(block) => {
                                methods::TransactionStatus::InBlock(block)
                            }
                            transactions_service::TransactionStatus::Retracted(block) => {
                                methods::TransactionStatus::Retracted(block)
                            }
                            transactions_service::TransactionStatus::Dropped => {
                                methods::TransactionStatus::Dropped
                            }
                            transactions_service::TransactionStatus::Finalized(block) => {
                                methods::TransactionStatus::Finalized(block)
                            }
                            transactions_service::TransactionStatus::FinalityTimeout(block) => {
                                methods::TransactionStatus::FinalityTimeout(block)
                            }
                        };

                        client.send_back(&smoldot::json_rpc::parse::build_subscription_event(
                            "author_extrinsicUpdate",
                            &subscription,
                            &serde_json::to_string(&update).unwrap(),
                        ));
                    }
                    future::Either::Right((Ok(unsub_request_id), _)) => {
                        let response = methods::Response::chain_unsubscribeNewHeads(true)
                            .to_json_response(&unsub_request_id);
                        client.send_back(&response);
                        break;
                    }
                    future::Either::Left((None, _)) => {
                        // Channel from the transactions service has been closed.
                        // Stop the task.
                        // There is nothing more that can be done except hope that the
                        // client understands that no new notification is expected and
                        // unsubscribes.
                        break;
                    }
                    future::Either::Right((Err(_), _)) => break,
                }
            }
        }));
    }

    /// Handles a call to [`methods::MethodCall::chain_getBlockHash`].
    async fn get_block_hash(self: Arc<JsonRpcService>, request_id: &str, height: Option<u64>) {
        let mut blocks = self.blocks.lock().await;
        let blocks = &mut *blocks;

        self.send_back(&match height {
            Some(0) => {
                methods::Response::chain_getBlockHash(methods::HashHexString(self.genesis_block))
                    .to_json_response(request_id)
            }
            None => {
                methods::Response::chain_getBlockHash(methods::HashHexString(blocks.best_block))
                    .to_json_response(request_id)
            }
            Some(n)
                if blocks
                    .known_blocks
                    .get(&blocks.best_block)
                    .map_or(false, |h| h.number == n) =>
            {
                methods::Response::chain_getBlockHash(methods::HashHexString(blocks.best_block))
                    .to_json_response(request_id)
            }
            Some(n)
                if blocks
                    .known_blocks
                    .get(&blocks.finalized_block)
                    .map_or(false, |h| h.number == n) =>
            {
                methods::Response::chain_getBlockHash(methods::HashHexString(
                    blocks.finalized_block,
                ))
                .to_json_response(request_id)
            }
            Some(_) => {
                // While the block could be found in `known_blocks`, there is no guarantee
                // that blocks in `known_blocks` are canonical, and we have no choice but to
                // return null.
                // TODO: ask a full node instead? or maybe keep a list of canonical blocks?
                json_rpc::parse::build_success_response(request_id, "null")
            }
        });
    }

    /// Handles a call to [`methods::MethodCall::chain_subscribeAllHeads`].
    async fn subscribe_all_heads(self: Arc<JsonRpcService>, request_id: &str) {
        let subscription = self
            .next_subscription
            .fetch_add(1, atomic::Ordering::Relaxed)
            .to_string();

        let (unsubscribe_tx, mut unsubscribe_rx) = oneshot::channel();
        self.all_heads
            .lock()
            .await
            .insert(subscription.clone(), unsubscribe_tx);

        let mut blocks_list = {
            // TODO: best blocks != all heads
            let (block_header, blocks_subscription) = self.sync_service.subscribe_best().await;
            stream::once(future::ready(block_header)).chain(blocks_subscription)
        };

        let confirmation =
            methods::Response::chain_subscribeAllHeads(&subscription).to_json_response(request_id);

        let client = self.clone();

        // Spawn a separate task for the subscription.
        (self.tasks_executor.lock().await)(Box::pin(async move {
            // Send back to the user the confirmation of the registration.
            client.send_back(&confirmation);

            loop {
                // Wait for either a new block, or for the subscription to be canceled.
                let next_block = blocks_list.next();
                futures::pin_mut!(next_block);
                match future::select(next_block, &mut unsubscribe_rx).await {
                    future::Either::Left((block, _)) => {
                        let header = header_conv(header::decode(&block.unwrap()).unwrap());
                        client.send_back(&smoldot::json_rpc::parse::build_subscription_event(
                            "chain_newHead",
                            &subscription,
                            &serde_json::to_string(&header).unwrap(),
                        ));
                    }
                    future::Either::Right((Ok(unsub_request_id), _)) => {
                        let response = methods::Response::chain_unsubscribeAllHeads(true)
                            .to_json_response(&unsub_request_id);
                        client.send_back(&response);
                        break;
                    }
                    future::Either::Right((Err(_), _)) => break,
                }
            }
        }));
    }

    /// Handles a call to [`methods::MethodCall::chain_subscribeNewHeads`].
    async fn subscribe_new_heads(self: Arc<JsonRpcService>, request_id: &str) {
        let subscription = self
            .next_subscription
            .fetch_add(1, atomic::Ordering::Relaxed)
            .to_string();

        let (unsubscribe_tx, mut unsubscribe_rx) = oneshot::channel();
        self.new_heads
            .lock()
            .await
            .insert(subscription.clone(), unsubscribe_tx);

        let mut blocks_list = {
            let (block_header, blocks_subscription) = self.sync_service.subscribe_best().await;
            stream::once(future::ready(block_header)).chain(blocks_subscription)
        };

        let confirmation =
            methods::Response::chain_subscribeNewHeads(&subscription).to_json_response(request_id);

        let client = self.clone();

        // Spawn a separate task for the subscription.
        (self.tasks_executor.lock().await)(Box::pin(async move {
            // Send back to the user the confirmation of the registration.
            client.send_back(&confirmation);

            loop {
                // Wait for either a new block, or for the subscription to be canceled.
                let next_block = blocks_list.next();
                futures::pin_mut!(next_block);
                match future::select(next_block, &mut unsubscribe_rx).await {
                    future::Either::Left((block, _)) => {
                        let header = header_conv(header::decode(&block.unwrap()).unwrap());
                        client.send_back(&smoldot::json_rpc::parse::build_subscription_event(
                            "chain_newHead",
                            &subscription,
                            &serde_json::to_string(&header).unwrap(),
                        ));
                    }
                    future::Either::Right((Ok(unsub_request_id), _)) => {
                        let response = methods::Response::chain_unsubscribeNewHeads(true)
                            .to_json_response(&unsub_request_id);
                        client.send_back(&response);
                        break;
                    }
                    future::Either::Right((Err(_), _)) => break,
                }
            }
        }));
    }

    /// Handles a call to [`methods::MethodCall::chain_subscribeFinalizedHeads`].
    async fn subscribe_finalized_heads(self: Arc<JsonRpcService>, request_id: &str) {
        let subscription = self
            .next_subscription
            .fetch_add(1, atomic::Ordering::Relaxed)
            .to_string();

        let (unsubscribe_tx, mut unsubscribe_rx) = oneshot::channel();
        self.finalized_heads
            .lock()
            .await
            .insert(subscription.clone(), unsubscribe_tx);

        let mut blocks_list = {
            let (finalized_block_header, finalized_blocks_subscription) =
                self.sync_service.subscribe_finalized().await;
            stream::once(future::ready(finalized_block_header)).chain(finalized_blocks_subscription)
        };

        let confirmation = methods::Response::chain_subscribeFinalizedHeads(&subscription)
            .to_json_response(request_id);

        let client = self.clone();

        // Spawn a separate task for the subscription.
        (self.tasks_executor.lock().await)(Box::pin(async move {
            // Send back to the user the confirmation of the registration.
            client.send_back(&confirmation);

            loop {
                // Wait for either a new block, or for the subscription to be canceled.
                let next_block = blocks_list.next();
                futures::pin_mut!(next_block);
                match future::select(next_block, &mut unsubscribe_rx).await {
                    future::Either::Left((block, _)) => {
                        let header = header_conv(header::decode(&block.unwrap()).unwrap());
                        client.send_back(&smoldot::json_rpc::parse::build_subscription_event(
                            "chain_finalizedHead",
                            &subscription,
                            &serde_json::to_string(&header).unwrap(),
                        ));
                    }
                    future::Either::Right((Ok(unsub_request_id), _)) => {
                        let response = methods::Response::chain_unsubscribeFinalizedHeads(true)
                            .to_json_response(&unsub_request_id);
                        client.send_back(&response);
                        break;
                    }
                    future::Either::Right((Err(_), _)) => break,
                }
            }
        }));
    }

    /// Handles a call to [`methods::MethodCall::state_subscribeStorage`].
    async fn subscribe_storage(
        self: Arc<JsonRpcService>,
        request_id: &str,
        list: Vec<methods::HexString>,
    ) {
        let subscription = self
            .next_subscription
            .fetch_add(1, atomic::Ordering::Relaxed)
            .to_string();

        let (unsubscribe_tx, mut unsubscribe_rx) = oneshot::channel();
        self.storage
            .lock()
            .await
            .insert(subscription.clone(), unsubscribe_tx);

        // Build a stream of `methods::StorageChangeSet` items to send back to the user.
        let storage_updates = {
            let known_values = (0..list.len()).map(|_| None).collect::<Vec<_>>();
            let client = self.clone();
            let (block_header, blocks_subscription) = self.sync_service.subscribe_best().await;
            let blocks_stream =
                stream::once(future::ready(block_header)).chain(blocks_subscription);

            stream::unfold(
                (blocks_stream, list, known_values),
                move |(mut blocks_stream, list, mut known_values)| {
                    let client = client.clone();
                    async move {
                        loop {
                            let block = blocks_stream.next().await?;
                            let block_hash = header::hash_from_scale_encoded_header(&block);
                            let state_trie_root = header::decode(&block).unwrap().state_root;

                            let mut out = methods::StorageChangeSet {
                                block: methods::HashHexString(block_hash),
                                changes: Vec::new(),
                            };

                            for (key_index, key) in list.iter().enumerate() {
                                // TODO: parallelism?
                                match client
                                    .network_service
                                    .clone()
                                    .storage_query(
                                        client.network_chain_index,
                                        &block_hash,
                                        state_trie_root,
                                        iter::once(&key.0),
                                    )
                                    .await
                                {
                                    Ok(mut values) => {
                                        let value = values.pop().unwrap();
                                        match &mut known_values[key_index] {
                                            Some(v) if *v == value => {}
                                            v @ _ => {
                                                *v = Some(value.clone());
                                                out.changes.push((
                                                    key.clone(),
                                                    value.map(methods::HexString),
                                                ));
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        log::log!(
                                            target: "json-rpc",
                                            if error.is_network_problem() { log::Level::Debug } else { log::Level::Warn },
                                            "state_subscribeStorage changes check failed: {}",
                                            error
                                        );
                                    }
                                }
                            }

                            if !out.changes.is_empty() {
                                return Some((out, (blocks_stream, list, known_values)));
                            }
                        }
                    }
                },
            )
        };

        let confirmation =
            methods::Response::state_subscribeStorage(&subscription).to_json_response(request_id);

        let client = self.clone();

        // Spawn a separate task for the subscription.
        (self.tasks_executor.lock().await)(Box::pin(async move {
            futures::pin_mut!(storage_updates);

            // Send back to the user the confirmation of the registration.
            client.send_back(&confirmation);

            loop {
                // Wait for either a new storage update, or for the subscription to be canceled.
                let next_block = storage_updates.next();
                futures::pin_mut!(next_block);
                match future::select(next_block, &mut unsubscribe_rx).await {
                    future::Either::Left((changes, _)) => {
                        client.send_back(&smoldot::json_rpc::parse::build_subscription_event(
                            "state_storage",
                            &subscription,
                            &serde_json::to_string(&changes).unwrap(),
                        ));
                    }
                    future::Either::Right((Ok(unsub_request_id), _)) => {
                        let response = methods::Response::state_unsubscribeStorage(true)
                            .to_json_response(&unsub_request_id);
                        client.send_back(&response);
                        break;
                    }
                    future::Either::Right((Err(_), _)) => break,
                }
            }
        }));
    }

    async fn storage_query(
        self: &Arc<JsonRpcService>,
        key: &[u8],
        hash: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, StorageQueryError> {
        // TODO: risk of deadlock here?
        let header = self
            .header_query(hash)
            .await
            .map_err(|_| StorageQueryError::FindStorageRootHashError)?;
        let trie_root_hash = header::decode(&header).unwrap().state_root;

        let mut result = self
            .network_service
            .clone()
            .storage_query(
                self.network_chain_index,
                hash,
                &trie_root_hash,
                iter::once(key),
            )
            .await
            .map_err(StorageQueryError::StorageRetrieval)?;
        Ok(result.pop().unwrap())
    }

    async fn header_query(self: &Arc<JsonRpcService>, hash: &[u8; 32]) -> Result<Vec<u8>, ()> {
        // TODO: risk of deadlock here?
        let mut blocks = self.blocks.lock().await;
        let blocks = &mut *blocks;

        if let Some(header) = blocks.known_blocks.get(hash) {
            Ok(header.scale_encoding_vec())
        } else {
            // Header isn't known locally. Ask the network.
            let result = self
                .network_service
                .clone()
                .block_query(
                    self.network_chain_index,
                    *hash,
                    protocol::BlocksRequestFields {
                        header: true,
                        body: false,
                        justification: false,
                    },
                )
                .await;

            // Note that the `block_query` method guarantees that the header is present
            // and valid.
            if let Ok(block) = result {
                Ok(block.header.unwrap())
            } else {
                Err(())
            }
        }
    }
}

#[derive(Debug, derive_more::Display)]
enum StorageQueryError {
    /// Error while finding the storage root hash of the requested block.
    #[display(fmt = "Unknown block")]
    FindStorageRootHashError,
    /// Error while retrieving the storage item from other nodes.
    #[display(fmt = "{}", _0)]
    StorageRetrieval(network_service::StorageQueryError),
}

impl StorageQueryError {
    /// Returns `true` if this is caused by networking issues, as opposed to a consensus-related
    /// issue.
    fn is_network_problem(&self) -> bool {
        match self {
            StorageQueryError::FindStorageRootHashError => true, // TODO: do properly
            StorageQueryError::StorageRetrieval(error) => error.is_network_problem(),
        }
    }
}

fn header_conv<'a>(header: impl Into<smoldot::header::HeaderRef<'a>>) -> methods::Header {
    let header = header.into();

    methods::Header {
        parent_hash: methods::HashHexString(*header.parent_hash),
        extrinsics_root: methods::HashHexString(*header.extrinsics_root),
        state_root: methods::HashHexString(*header.state_root),
        number: header.number,
        digest: methods::HeaderDigest {
            logs: header
                .digest
                .logs()
                .map(|log| {
                    methods::HexString(log.scale_encoding().fold(Vec::new(), |mut a, b| {
                        a.extend_from_slice(b.as_ref());
                        a
                    }))
                })
                .collect(),
        },
    }
}
