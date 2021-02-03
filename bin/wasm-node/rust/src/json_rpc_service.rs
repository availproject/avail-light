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

// TODO: doc
// TODO: re-review this once finished

use crate::{ffi, network_service, sync_service};

use futures::{channel::oneshot, lock::Mutex, prelude::*};
use methods::MethodCall;
use smoldot::{
    chain_spec, executor, header,
    json_rpc::{self, methods},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    convert::TryFrom as _,
    pin::Pin,
    sync::{atomic, Arc},
};

/// Configuration for a JSON-RPC service.
pub struct Config {
    /// Closure that spawns background tasks.
    pub tasks_executor: Box<dyn FnMut(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,

    /// Service responsible for the networking of the chain.
    pub network_service: Arc<network_service::NetworkService>,

    /// Service responsible for synchronizing the chain.
    pub sync_service: Arc<sync_service::SyncService>,

    /// Specifications of the chain.
    pub chain_spec: chain_spec::ChainSpec,

    /// Hash of the genesis block of the chain.
    ///
    /// > **Note**: This can be derived from a [`chain_spec::ChainSpec`]. While the [`start`]
    /// >           function could in theory use the [`Config::chain_spec`] parameter to derive
    /// >           this value, doing so is quite expensive. We prefer to require this value
    /// >           from the upper layer instead, as it is most likely needed anyway.
    pub genesis_block_hash: [u8; 32],
}

/// Initializes the JSON-RPC service with the given configuration.
///
/// It will run in the background, pulling events using [`ffi::next_json_rpc`] and sending back
/// answers using [`ffi::emit_json_rpc_response`].
/// Because of the racy nature of these two functions, it is strongly discouraged to spawn
/// multiple JSON-RPC services, especially if they don't use the same
/// [`sync_service::SyncService`].
pub async fn start(config: Config) {
    let genesis_storage = config
        .chain_spec
        .genesis_storage()
        .map(|(k, v)| (k.to_vec(), v.to_vec()))
        .collect::<BTreeMap<_, _>>();

    // TODO: do this in a cleaner way
    let best_block_metadata = {
        let code = genesis_storage.get(&b":code"[..]).unwrap();
        let heap_pages = 1024; // TODO: laziness
        smoldot::metadata::metadata_from_runtime_code(code, heap_pages).unwrap()
    };

    // TODO: do this in a cleaner way
    let best_block_runtime_spec = {
        let vm = executor::host::HostVmPrototype::new(
            &genesis_storage.get(&b":code"[..]).unwrap(),
            executor::DEFAULT_HEAP_PAGES,
            executor::vm::ExecHint::Oneshot,
        )
        .unwrap();
        executor::core_version(vm).unwrap().0
    };

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
        network_service: config.network_service,
        sync_service: config.sync_service,
        blocks: Mutex::new(Blocks {
            known_blocks,
            best_block: best_block_hash,
            finalized_block: best_block_hash,
        }),
        genesis_block: config.genesis_block_hash,
        genesis_storage,
        best_block_metadata: Mutex::new(best_block_metadata),
        best_block_runtime_spec,
        next_subscription: atomic::AtomicU64::new(0),
        runtime_version: HashSet::new(),
        all_heads: Mutex::new(HashMap::new()),
        new_heads: Mutex::new(HashMap::new()),
        finalized_heads: Mutex::new(HashMap::new()),
        storage: Mutex::new(HashMap::new()),
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

    (client.clone().tasks_executor.lock().await)(Box::pin(async move {
        loop {
            let json_rpc_request = ffi::next_json_rpc().await;

            // Each incoming request gets its own separate task.
            let client2 = client.clone();
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

                let (response1, response2) = client2.clone().handle_rpc(request_id, call).await;

                if let Some(response1) = response1 {
                    log::debug!(
                        target: "json-rpc",
                        "JSON-RPC <= {:?}{}",
                        if response1.len() > 100 { &response1[..100] } else { &response1[..] },
                        if response1.len() > 100 { "…" } else { "" }
                    );
                    ffi::emit_json_rpc_response(&response1);
                }

                if let Some(response2) = response2 {
                    log::debug!(
                        target: "json-rpc",
                        "JSON-RPC <= {:?}{}",
                        if response2.len() > 100 { &response2[..100] } else { &response2[..] },
                        if response2.len() > 100 { "…" } else { "" }
                    );
                    ffi::emit_json_rpc_response(&response2);
                }
            }));
        }
    }));
}

struct JsonRpcService {
    /// See [`Config::tasks_executor`].
    tasks_executor: Mutex<Box<dyn FnMut(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>>,

    chain_spec: chain_spec::ChainSpec,

    network_service: Arc<network_service::NetworkService>,
    sync_service: Arc<sync_service::SyncService>,

    /// Blocks that are temporarily saved in order to serve JSON-RPC requests.
    blocks: Mutex<Blocks>,

    /// Hash of the genesis block.
    /// Keeping the genesis block is important, as the genesis block hash is included in
    /// transaction signatures, and must therefore be queried by upper-level UIs.
    genesis_block: [u8; 32],

    // TODO: remove; unnecessary
    genesis_storage: BTreeMap<Vec<u8>, Vec<u8>>,

    best_block_metadata: Mutex<Vec<u8>>,
    best_block_runtime_spec: executor::CoreVersion,

    next_subscription: atomic::AtomicU64,

    runtime_version: HashSet<String>,

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
    // TODO: remove second tuple item of the return value, once possible
    async fn handle_rpc(
        self: Arc<JsonRpcService>,
        request_id: &str,
        call: MethodCall,
    ) -> (Option<String>, Option<String>) {
        match call {
            methods::MethodCall::author_pendingExtrinsics {} => {
                let response = methods::Response::author_pendingExtrinsics(Vec::new())
                    .to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::author_submitExtrinsic { transaction } => {
                let response = match self
                    .network_service
                    .clone()
                    .announce_transaction(&transaction.0)
                    .await
                {
                    Ok(_) => {
                        let mut hash_context = blake2_rfc::blake2b::Blake2b::new(32);
                        hash_context.update(transaction.0.as_slice());
                        let mut transaction_hash: [u8; 32] = Default::default();
                        transaction_hash.copy_from_slice(hash_context.finalize().as_bytes());

                        methods::Response::author_submitExtrinsic(methods::HashHexString(
                            transaction_hash,
                        ))
                        .to_json_response(request_id)
                    }
                    Err(error) => json_rpc::parse::build_error_response(
                        request_id,
                        json_rpc::parse::ErrorResponse::ServerError(-32000, &format!("{}", &error)),
                        None,
                    ),
                };
                (Some(response), None)
            }
            methods::MethodCall::chain_getBlockHash { height } => {
                let mut blocks = self.blocks.lock().await;
                let blocks = &mut *blocks;

                let response = match height {
                    Some(0) => methods::Response::chain_getBlockHash(methods::HashHexString(
                        self.genesis_block,
                    ))
                    .to_json_response(request_id),
                    None => methods::Response::chain_getBlockHash(methods::HashHexString(
                        blocks.best_block,
                    ))
                    .to_json_response(request_id),
                    Some(n)
                        if blocks
                            .known_blocks
                            .get(&blocks.best_block)
                            .map_or(false, |h| h.number == n) =>
                    {
                        methods::Response::chain_getBlockHash(methods::HashHexString(
                            blocks.best_block,
                        ))
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
                };

                (Some(response), None)
            }
            methods::MethodCall::chain_getFinalizedHead {} => {
                let response = methods::Response::chain_getFinalizedHead(methods::HashHexString(
                    self.blocks.lock().await.finalized_block,
                ))
                .to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::chain_getHeader { hash } => {
                let mut blocks = self.blocks.lock().await;
                let blocks = &mut *blocks;
                let hash = hash.as_ref().map(|h| &h.0).unwrap_or(&blocks.best_block);
                let response = if let Some(header) = blocks.known_blocks.get(hash) {
                    methods::Response::chain_getHeader(header_conv(header))
                        .to_json_response(request_id)
                } else {
                    json_rpc::parse::build_success_response(request_id, "null")
                };

                (Some(response), None)
            }
            methods::MethodCall::chain_subscribeAllHeads {} => {
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
                    let (block_header, blocks_subscription) =
                        self.sync_service.subscribe_best().await;
                    stream::once(future::ready(block_header)).chain(blocks_subscription)
                };

                let confirmation = methods::Response::chain_subscribeAllHeads(&subscription)
                    .to_json_response(request_id);

                // Spawn a separate task for the subscription.
                (self.tasks_executor.lock().await)(Box::pin(async move {
                    // Send back to the user the confirmation of the registration.
                    ffi::emit_json_rpc_response(&confirmation);

                    loop {
                        // Wait for either a new block, or for the subscription to be canceled.
                        let next_block = blocks_list.next();
                        futures::pin_mut!(next_block);
                        match future::select(next_block, &mut unsubscribe_rx).await {
                            future::Either::Left((block, _)) => {
                                let header = header_conv(header::decode(&block.unwrap()).unwrap());
                                ffi::emit_json_rpc_response(
                                    &smoldot::json_rpc::parse::build_subscription_event(
                                        "chain_newHead",
                                        &subscription,
                                        &serde_json::to_string(&header).unwrap(),
                                    ),
                                );
                            }
                            future::Either::Right((Ok(unsub_request_id), _)) => {
                                let response = methods::Response::chain_unsubscribeAllHeads(true)
                                    .to_json_response(&unsub_request_id);
                                ffi::emit_json_rpc_response(&response);
                                break;
                            }
                            future::Either::Right((Err(_), _)) => break,
                        }
                    }
                }));

                (None, None)
            }
            methods::MethodCall::chain_subscribeNewHeads {} => {
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
                    let (block_header, blocks_subscription) =
                        self.sync_service.subscribe_best().await;
                    stream::once(future::ready(block_header)).chain(blocks_subscription)
                };

                let confirmation = methods::Response::chain_subscribeNewHeads(&subscription)
                    .to_json_response(request_id);

                // Spawn a separate task for the subscription.
                (self.tasks_executor.lock().await)(Box::pin(async move {
                    // Send back to the user the confirmation of the registration.
                    ffi::emit_json_rpc_response(&confirmation);

                    loop {
                        // Wait for either a new block, or for the subscription to be canceled.
                        let next_block = blocks_list.next();
                        futures::pin_mut!(next_block);
                        match future::select(next_block, &mut unsubscribe_rx).await {
                            future::Either::Left((block, _)) => {
                                let header = header_conv(header::decode(&block.unwrap()).unwrap());
                                ffi::emit_json_rpc_response(
                                    &smoldot::json_rpc::parse::build_subscription_event(
                                        "chain_newHead",
                                        &subscription,
                                        &serde_json::to_string(&header).unwrap(),
                                    ),
                                );
                            }
                            future::Either::Right((Ok(unsub_request_id), _)) => {
                                let response = methods::Response::chain_unsubscribeNewHeads(true)
                                    .to_json_response(&unsub_request_id);
                                ffi::emit_json_rpc_response(&response);
                                break;
                            }
                            future::Either::Right((Err(_), _)) => break,
                        }
                    }
                }));

                (None, None)
            }
            methods::MethodCall::chain_subscribeFinalizedHeads {} => {
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
                    stream::once(future::ready(finalized_block_header))
                        .chain(finalized_blocks_subscription)
                };

                let confirmation = methods::Response::chain_subscribeFinalizedHeads(&subscription)
                    .to_json_response(request_id);

                // Spawn a separate task for the subscription.
                (self.tasks_executor.lock().await)(Box::pin(async move {
                    // Send back to the user the confirmation of the registration.
                    ffi::emit_json_rpc_response(&confirmation);

                    loop {
                        // Wait for either a new block, or for the subscription to be canceled.
                        let next_block = blocks_list.next();
                        futures::pin_mut!(next_block);
                        match future::select(next_block, &mut unsubscribe_rx).await {
                            future::Either::Left((block, _)) => {
                                let header = header_conv(header::decode(&block.unwrap()).unwrap());
                                ffi::emit_json_rpc_response(
                                    &smoldot::json_rpc::parse::build_subscription_event(
                                        "chain_finalizedHead",
                                        &subscription,
                                        &serde_json::to_string(&header).unwrap(),
                                    ),
                                );
                            }
                            future::Either::Right((Ok(unsub_request_id), _)) => {
                                let response =
                                    methods::Response::chain_unsubscribeFinalizedHeads(true)
                                        .to_json_response(&unsub_request_id);
                                ffi::emit_json_rpc_response(&response);
                                break;
                            }
                            future::Either::Right((Err(_), _)) => break,
                        }
                    }
                }));

                (None, None)
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
                    let response = methods::Response::chain_unsubscribeFinalizedHeads(false)
                        .to_json_response(request_id);
                    (Some(response), None)
                } else {
                    (None, None)
                }
            }
            methods::MethodCall::payment_queryInfo { extrinsic, hash } => {
                assert!(hash.is_none()); // TODO:
                                         // TODO: complete hack
                let response = methods::Response::payment_queryInfo(methods::RuntimeDispatchInfo {
                    weight: 220429000,                     // TODO: no
                    class: methods::DispatchClass::Normal, // TODO: no
                    partial_fee: 15600000001,              // TODO: no
                })
                .to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::rpc_methods {} => {
                let response = methods::Response::rpc_methods(methods::RpcMethods {
                    version: 1,
                    methods: methods::MethodCall::method_names()
                        .map(|n| n.into())
                        .collect(),
                })
                .to_json_response(request_id);
                (Some(response), None)
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

                let response =
                    methods::Response::state_queryStorageAt(vec![out]).to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::state_getKeysPaged {
                prefix,
                count,
                start_key,
                hash,
            } => {
                assert!(hash.is_none()); // TODO:

                let mut out = Vec::new();
                // TODO: check whether start_key should be included of the set
                for (k, _) in self
                    .genesis_storage
                    .range(start_key.map(|p| p.0).unwrap_or(Vec::new())..)
                {
                    if out.len() >= usize::try_from(count).unwrap_or(usize::max_value()) {
                        break;
                    }

                    if prefix
                        .as_ref()
                        .map_or(false, |prefix| !k.starts_with(&prefix.0))
                    {
                        break;
                    }

                    out.push(methods::HexString(k.to_vec()));
                }

                let response =
                    methods::Response::state_getKeysPaged(out).to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::state_getMetadata {} => {
                let best_block_hash = self.blocks.lock().await.best_block;
                let response = match self.storage_query(&b":code"[..], &best_block_hash).await {
                    Ok(Some(value)) => {
                        let best_block_metadata = {
                            let heap_pages = executor::DEFAULT_HEAP_PAGES; // TODO: laziness
                            smoldot::metadata::metadata_from_runtime_code(&value, heap_pages)
                                .unwrap()
                        };

                        *self.best_block_metadata.lock().await = best_block_metadata;

                        methods::Response::state_getMetadata(methods::HexString(
                            self.best_block_metadata.lock().await.clone(),
                        ))
                        .to_json_response(request_id)
                    }
                    Ok(None) => json_rpc::parse::build_success_response(request_id, "null"),
                    Err(_) => {
                        // Return the last known best_block_metadata
                        methods::Response::state_getMetadata(methods::HexString(
                            self.best_block_metadata.lock().await.clone(),
                        ))
                        .to_json_response(request_id)
                    }
                };

                (Some(response), None)
            }
            methods::MethodCall::state_getStorage { key, hash } => {
                let hash = hash
                    .as_ref()
                    .map(|h| h.0)
                    .unwrap_or(self.blocks.lock().await.best_block);

                let response = match self.storage_query(&key.0, &hash).await {
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
                };

                (Some(response), None)
            }
            methods::MethodCall::state_subscribeRuntimeVersion {} => {
                let subscription = self
                    .next_subscription
                    .fetch_add(1, atomic::Ordering::Relaxed)
                    .to_string();

                let response = methods::Response::state_subscribeRuntimeVersion(&subscription)
                    .to_json_response(request_id);
                // TODO: subscription has no effect right now
                // TODO: self.runtime_version.insert(subscription.clone());

                let best_block_hash = self.blocks.lock().await.best_block;
                let runtime_code = self.storage_query(&b":code"[..], &best_block_hash).await;
                let runtime_specs = if let Ok(runtime_code) = runtime_code {
                    // TODO: don't unwrap
                    // TODO: cache the VM
                    let vm = executor::host::HostVmPrototype::new(
                        &runtime_code.unwrap(),
                        executor::DEFAULT_HEAP_PAGES,
                        executor::vm::ExecHint::Oneshot,
                    )
                    .unwrap();
                    executor::core_version(vm).unwrap().0
                } else {
                    self.best_block_runtime_spec.clone()
                };

                let runtime_specs = runtime_specs.decode();
                let response2 = smoldot::json_rpc::parse::build_subscription_event(
                    "state_runtimeVersion",
                    &subscription,
                    &serde_json::to_string(&methods::RuntimeVersion {
                        spec_name: runtime_specs.spec_name.into(),
                        impl_name: runtime_specs.impl_name.into(),
                        authoring_version: u64::from(runtime_specs.authoring_version),
                        spec_version: u64::from(runtime_specs.spec_version),
                        impl_version: u64::from(runtime_specs.impl_version),
                        transaction_version: runtime_specs.transaction_version.map(u64::from),
                        apis: runtime_specs.apis,
                    })
                    .unwrap(),
                );
                (Some(response), Some(response2))
            }
            methods::MethodCall::state_subscribeStorage { list } => {
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
                    let (block_header, blocks_subscription) =
                        self.sync_service.subscribe_best().await;
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
                                    let state_trie_root =
                                        header::decode(&block).unwrap().state_root;

                                    let mut out = methods::StorageChangeSet {
                                        block: methods::HashHexString(block_hash),
                                        changes: Vec::new(),
                                    };

                                    for (key_index, key) in list.iter().enumerate() {
                                        // TODO: parallelism?
                                        match client
                                            .network_service
                                            .clone()
                                            .storage_query(&block_hash, state_trie_root, &key.0)
                                            .await
                                        {
                                            Ok(value) => match &mut known_values[key_index] {
                                                Some(v) if *v == value => {}
                                                v @ _ => {
                                                    *v = Some(value.clone());
                                                    out.changes.push((
                                                        key.clone(),
                                                        value.map(methods::HexString),
                                                    ));
                                                }
                                            },
                                            Err(error) => {
                                                log::warn!(
                                                    target: "json-rpc",
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

                let confirmation = methods::Response::state_subscribeStorage(&subscription)
                    .to_json_response(request_id);

                // Spawn a separate task for the subscription.
                (self.tasks_executor.lock().await)(Box::pin(async move {
                    futures::pin_mut!(storage_updates);

                    // Send back to the user the confirmation of the registration.
                    ffi::emit_json_rpc_response(&confirmation);

                    loop {
                        // Wait for either a new storage update, or for the subscription to be canceled.
                        let next_block = storage_updates.next();
                        futures::pin_mut!(next_block);
                        match future::select(next_block, &mut unsubscribe_rx).await {
                            future::Either::Left((changes, _)) => {
                                ffi::emit_json_rpc_response(
                                    &smoldot::json_rpc::parse::build_subscription_event(
                                        "state_storage",
                                        &subscription,
                                        &serde_json::to_string(&changes).unwrap(),
                                    ),
                                );
                            }
                            future::Either::Right((Ok(unsub_request_id), _)) => {
                                let response = methods::Response::state_unsubscribeStorage(true)
                                    .to_json_response(&unsub_request_id);
                                ffi::emit_json_rpc_response(&response);
                                break;
                            }
                            future::Either::Right((Err(_), _)) => break,
                        }
                    }
                }));

                (None, None)
            }
            methods::MethodCall::state_unsubscribeStorage { subscription } => {
                let invalid =
                    if let Some(cancel_tx) = self.storage.lock().await.remove(&subscription) {
                        cancel_tx.send(request_id.to_owned()).is_err()
                    } else {
                        true
                    };

                if invalid {
                    let response = methods::Response::state_unsubscribeStorage(false)
                        .to_json_response(request_id);
                    (Some(response), None)
                } else {
                    (None, None)
                }
            }
            methods::MethodCall::state_getRuntimeVersion {} => {
                let best_block_hash = self.blocks.lock().await.best_block;
                let runtime_code = self.storage_query(&b":code"[..], &best_block_hash).await;
                let runtime_specs = if let Ok(runtime_code) = runtime_code {
                    // TODO: don't unwrap
                    // TODO: cache the VM
                    let vm = executor::host::HostVmPrototype::new(
                        &runtime_code.unwrap(),
                        executor::DEFAULT_HEAP_PAGES,
                        executor::vm::ExecHint::Oneshot,
                    )
                    .unwrap();
                    executor::core_version(vm).unwrap().0
                } else {
                    self.best_block_runtime_spec.clone()
                };

                let runtime_specs = runtime_specs.decode();
                let response =
                    methods::Response::state_getRuntimeVersion(methods::RuntimeVersion {
                        spec_name: runtime_specs.spec_name.into(),
                        impl_name: runtime_specs.impl_name.into(),
                        authoring_version: u64::from(runtime_specs.authoring_version),
                        spec_version: u64::from(runtime_specs.spec_version),
                        impl_version: u64::from(runtime_specs.impl_version),
                        transaction_version: runtime_specs.transaction_version.map(u64::from),
                        apis: runtime_specs.apis,
                    })
                    .to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::system_accountNextIndex { account } => {
                // TODO: implement
                let response = json_rpc::parse::build_error_response(
                    request_id,
                    json_rpc::parse::ErrorResponse::ServerError(-32000, "Unsupported call"),
                    None,
                );
                (Some(response), None)
            }
            methods::MethodCall::system_chain {} => {
                let response = methods::Response::system_chain(self.chain_spec.name())
                    .to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::system_chainType {} => {
                let response = methods::Response::system_chainType(self.chain_spec.chain_type())
                    .to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::system_health {} => {
                let response = methods::Response::system_health(methods::SystemHealth {
                    is_syncing: !self.sync_service.is_above_network_finalized().await,
                    peers: u64::try_from(self.network_service.peers_list().await.count())
                        .unwrap_or(u64::max_value()),
                    should_have_peers: self.chain_spec.has_live_network(),
                })
                .to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::system_name {} => {
                let response =
                    methods::Response::system_name("smoldot!").to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::system_peers {} => {
                // TODO: return proper response
                let response = methods::Response::system_peers(
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
                .to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::system_properties {} => {
                let response = methods::Response::system_properties(
                    serde_json::from_str(self.chain_spec.properties()).unwrap(),
                )
                .to_json_response(request_id);
                (Some(response), None)
            }
            methods::MethodCall::system_version {} => {
                let response =
                    methods::Response::system_version("1.0.0").to_json_response(request_id);
                (Some(response), None)
            }
            _method => {
                log::error!(target: "json-rpc", "JSON-RPC call not supported yet: {:?}", _method);
                let response = json_rpc::parse::build_error_response(
                    request_id,
                    json_rpc::parse::ErrorResponse::ServerError(
                        -32000,
                        "Not implemented in smoldot yet",
                    ),
                    None,
                );
                (Some(response), None)
            }
        }
    }

    async fn storage_query(
        self: &Arc<JsonRpcService>,
        key: &[u8],
        hash: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, StorageQueryError> {
        // TODO: risk of deadlock here?
        let trie_root_hash = if let Some(header) = self.blocks.lock().await.known_blocks.get(hash) {
            header.state_root
        } else {
            // TODO: should make a block request towards a node
            return Err(StorageQueryError::FindStorageRootHashError);
        };

        self.network_service
            .clone()
            .storage_query(hash, &trie_root_hash, key)
            .await
            .map_err(StorageQueryError::StorageRetrieval)
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
