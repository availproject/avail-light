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

use crate::{ffi, network_service, sync_service};

use futures::{channel::oneshot, lock::Mutex, prelude::*};
use methods::MethodCall;
use smoldot::{
    chain_spec, executor, header,
    json_rpc::{self, methods},
    metadata,
    network::protocol,
    trie::proof_verify,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    convert::TryFrom as _,
    iter,
    pin::Pin,
    sync::{atomic, Arc},
    time::Duration,
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
    // TODO: remove; this BTreeMap serves no purpose except convenience
    let genesis_storage = config
        .chain_spec
        .genesis_storage()
        .map(|(k, v)| (k.to_vec(), v.to_vec()))
        .collect::<BTreeMap<_, _>>();

    let latest_known_runtime = {
        let code = genesis_storage.get(&b":code"[..]).map(|v| v.to_vec());
        let heap_pages = genesis_storage.get(&b":heappages"[..]).map(|v| v.to_vec());

        // Note that in the absolute we don't need to panic in case of a problem, and could simply
        // store an `Err` and continue running.
        // However, in practice, it seems more sane to detect problems in the genesis block.
        let runtime = SuccessfulRuntime::from_params(&code, &heap_pages)
            .expect("invalid runtime at genesis block");
        LatestKnownRuntime {
            runtime: Ok(runtime),
            runtime_code: code,
            heap_pages,
            runtime_block_hash: config.genesis_block_hash,
            runtime_block_state_root: config.genesis_block_state_root,
            runtime_version_subscriptions: HashSet::new(),
        }
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
        latest_known_runtime: Mutex::new(latest_known_runtime),
        next_subscription: atomic::AtomicU64::new(0),
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

    // Spawns a task that downloads the runtime code at every block to check whether it has
    // changed.
    //
    // This is strictly speaking not necessary as long as there is no active runtime specs
    // subscription. However, in practice, there is most likely always going to be one. It is
    // easier to always have a task active rather than create and destroy it.
    (client.clone().tasks_executor.lock().await)({
        let client = client.clone();
        let blocks_stream = {
            let (best_block_header, best_blocks_subscription) =
                client.sync_service.subscribe_best().await;
            stream::once(future::ready(best_block_header)).chain(best_blocks_subscription)
        };

        // Set to `true` when we expect the runtime in `latest_known_runtime` to match the runtime
        // of the best block. Initially `false`, as `latest_known_runtime` uses the genesis
        // runtime.
        let mut runtime_matches_best_block = false;

        Box::pin(async move {
            futures::pin_mut!(blocks_stream);

            loop {
                // While major-syncing a chain, best blocks are updated continously. In that
                // situation, the delay below is too short to prevent the runtime code from being
                // continuously downloaded.
                // To avoid using too much bandwidth, we force another delay between two runtime
                // code downloads.
                // This delay is done at the beginning of the loop because the runtime is built
                // as part of the initialization of the `JsonRpcService`, and in order to make it
                // possible to use `continue` without accidentally skipping this delay.
                ffi::Delay::new(Duration::from_secs(3)).await;

                // Wait until a new best block is known.
                let mut new_best_block = match blocks_stream.next().await {
                    Some(b) => b,
                    None => break, // Stream is finished.
                };

                // While the chain is running, it is often the case that more than one blocks
                // is generated and announced roughly at the same time.
                // We would like to avoid a situation where we receive a new best block, start
                // downloading the runtime code, then a few milliseconds later receive another
                // block that becomes the new best, and download the runtime code of that new
                // block as well. This would lead to downloading the runtime code twice (or more,
                // if more than two blocks are received) in a small time frame, which is usually a
                // waste of bandwidth.
                // Instead, whenever a new best block is received, we wait a little bit before
                // downloading the runtime, in order to see if there isn't any other new best
                // block already on the way.
                // This delay needs to be long enough to de-duplicate forks, but it should still
                // be small, as it adds artifical latency to the detecting runtime upgrades.
                ffi::Delay::new(Duration::from_millis(500)).await;
                while let Some(best_update) = blocks_stream.next().now_or_never() {
                    new_best_block = match best_update {
                        Some(b) => b,
                        None => break, // Stream is finished.
                    };
                }

                // Download the runtime code of this new best block.
                let new_best_block_decoded = header::decode(&new_best_block).unwrap();
                let new_best_block_hash = header::hash_from_scale_encoded_header(&new_best_block);
                let (new_code, new_heap_pages) = {
                    let mut results = match client
                        .network_service
                        .clone()
                        .storage_query(
                            &new_best_block_hash,
                            new_best_block_decoded.state_root,
                            iter::once(&b":code"[..]).chain(iter::once(&b":heappages"[..])),
                        )
                        .await
                    {
                        Ok(c) => c,
                        Err(error) => {
                            log::warn!(
                                target: "json-rpc",
                                "Failed to download :code and :heappages of new best block: {}",
                                error
                            );
                            continue;
                        }
                    };

                    let new_heap_pages = results.pop().unwrap();
                    let new_code = results.pop().unwrap();
                    (new_code, new_heap_pages)
                };

                // Only lock `latest_known_runtime` now that everything is synchronous.
                let mut latest_known_runtime = client.latest_known_runtime.lock().await;

                // `runtime_block_hash` is always updated in order to have the most recent
                // block possible.
                latest_known_runtime.runtime_block_hash = new_best_block_hash;
                latest_known_runtime.runtime_block_state_root = *new_best_block_decoded.state_root;

                // `continue` if there wasn't any change in `:code` and `:heappages`.
                if new_code == latest_known_runtime.runtime_code
                    && new_heap_pages == latest_known_runtime.heap_pages
                {
                    runtime_matches_best_block = true;
                    continue;
                }

                // Don't notify the user of an upgrade if we didn't expect the runtime to match
                // the best block in the first place.
                if runtime_matches_best_block {
                    log::info!(
                        target: "json-rpc",
                        "New runtime code detected around block #{} (block number might be wrong)",
                        new_best_block_decoded.number
                    );
                }

                runtime_matches_best_block = true;
                latest_known_runtime.runtime_code = new_code;
                latest_known_runtime.heap_pages = new_heap_pages;
                latest_known_runtime.runtime = SuccessfulRuntime::from_params(
                    &latest_known_runtime.runtime_code,
                    &latest_known_runtime.heap_pages,
                );

                let notification_body = if let Ok(runtime) = &latest_known_runtime.runtime {
                    let runtime_spec = runtime.runtime_spec.decode();
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

                for subscription in &latest_known_runtime.runtime_version_subscriptions {
                    let notification = smoldot::json_rpc::parse::build_subscription_event(
                        "state_runtimeVersion",
                        &subscription,
                        &notification_body,
                    );
                    client.send_back(&notification);
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
                    client2.send_back(&response1);
                }

                if let Some(response2) = response2 {
                    client2.send_back(&response2);
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

    /// Initially contains the runtime code of the genesis block. Whenever a best block is
    /// received, updated with the runtime of this new best block.
    /// If, after a new best block, it isn't possible to determine whether the runtime has changed,
    /// the content will be left unchanged. However, if an error happens for example when compiling
    /// the new runtime, then the content will contain an error.
    latest_known_runtime: Mutex<LatestKnownRuntime>,

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
}

struct LatestKnownRuntime {
    /// Successfully-compiled runtime and all its information. Can contain an error if an error
    /// happened, including a problem when obtaining the runtime specs or the metadata. It is
    /// better to report to the user an error about for example the metadata not being extractable
    /// compared to returning an obsolete version.
    runtime: Result<SuccessfulRuntime, ()>,

    /// Undecoded storage value of `:code` corresponding to the [`LatestKnownRuntime::runtime`]
    /// field.
    runtime_code: Option<Vec<u8>>,
    /// Undecoded storage value of `:heappages` corresponding to the
    /// [`LatestKnownRuntime::runtime`] field.
    heap_pages: Option<Vec<u8>>,
    /// Hash of a block known to have the runtime found in the [`LatestKnownRuntime::runtime`]
    /// field. Always updated to a recent block having this runtime.
    runtime_block_hash: [u8; 32],
    /// Storage trie root of the block whose hash is [`LatestKnownRuntime::runtime_block_hash`].
    runtime_block_state_root: [u8; 32],

    /// List of active subscriptions for runtime version updates.
    /// Whenever [`LatestKnownRuntime::runtime`] is updated, one should emit a notification
    /// regarding these subscriptions.
    runtime_version_subscriptions: HashSet<String>,
}

struct SuccessfulRuntime {
    /// Metadata extracted from the runtime.
    metadata: Vec<u8>,

    /// Runtime specs extracted from the runtime.
    runtime_spec: executor::CoreVersion,

    /// Virtual machine itself, to perform additional calls.
    ///
    /// Always `Some`, except for temporary extractions. Should always be `Some`, when the
    /// [`SuccessfulRuntime`] is accessed.
    virtual_machine: Option<executor::host::HostVmPrototype>,
}

impl SuccessfulRuntime {
    fn from_params(code: &Option<Vec<u8>>, heap_pages: &Option<Vec<u8>>) -> Result<Self, ()> {
        let vm = match executor::host::HostVmPrototype::new(
            code.as_ref().ok_or(())?,
            executor::DEFAULT_HEAP_PAGES, // TODO: proper heap pages
            executor::vm::ExecHint::CompileAheadOfTime,
        ) {
            Ok(vm) => vm,
            Err(error) => {
                log::warn!(target: "json-rpc", "Failed to compile best block runtime: {}", error);
                return Err(());
            }
        };

        let (runtime_spec, vm) = match executor::core_version(vm) {
            Ok(v) => v,
            Err(_error) => {
                log::warn!(
                    target: "json-rpc",
                    "Failed to call Core_version on new runtime",  // TODO: print error message as well ; at the moment the type of the error is `()`
                );
                return Err(());
            }
        };

        let (metadata, vm) = match metadata::metadata_from_virtual_machine_prototype(vm) {
            Ok(v) => v,
            Err(error) => {
                log::warn!(
                    target: "json-rpc",
                    "Failed to call Metadata_metadata on new runtime: {}",
                    error
                );
                return Err(());
            }
        };

        Ok(SuccessfulRuntime {
            metadata,
            runtime_spec,
            virtual_machine: Some(vm),
        })
    }
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
                                client.send_back(
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
                                client.send_back(&response);
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
                                client.send_back(
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
                                client.send_back(&response);
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
                                client.send_back(
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
                                client.send_back(&response);
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
                assert!(hash.is_none()); // TODO: handle when hash != None
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
                let latest_known_runtime = self.latest_known_runtime.lock().await;

                // `runtime` is `Ok` if the latest known runtime is valid, and `Err` if it isn't.
                let response = if let Ok(runtime) = &latest_known_runtime.runtime {
                    methods::Response::state_getMetadata(methods::HexString(
                        runtime.metadata.clone(), // TODO: expensive clone
                    ))
                    .to_json_response(request_id)
                } else {
                    json_rpc::parse::build_error_response(
                        request_id,
                        json_rpc::parse::ErrorResponse::ServerError(-32000, "Invalid runtime"),
                        None,
                    )
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

                let mut latest_known_runtime = self.latest_known_runtime.lock().await;
                latest_known_runtime
                    .runtime_version_subscriptions
                    .insert(subscription.clone());

                let response = methods::Response::state_subscribeRuntimeVersion(&subscription)
                    .to_json_response(request_id);

                let notification = if let Ok(runtime) = &latest_known_runtime.runtime {
                    let runtime_spec = runtime.runtime_spec.decode();
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

                let response2 = smoldot::json_rpc::parse::build_subscription_event(
                    "state_runtimeVersion",
                    &subscription,
                    &notification,
                );

                // TODO: in theory it is possible for the subscription to take effect and return
                // a notification before these responses are sent back ; should be fixed by more refactoring

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
                                            .storage_query(
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
                                client.send_back(
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
                                client.send_back(&response);
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
                let latest_known_runtime = self.latest_known_runtime.lock().await;
                let response = if let Ok(runtime) = &latest_known_runtime.runtime {
                    let runtime_spec = runtime.runtime_spec.decode();
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
                };

                (Some(response), None)
            }
            methods::MethodCall::system_accountNextIndex { account } => {
                let response = match self
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
                };

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
                    methods::Response::system_name("smoldot").to_json_response(request_id);
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

        let mut result = self
            .network_service
            .clone()
            .storage_query(hash, &trie_root_hash, iter::once(key))
            .await
            .map_err(StorageQueryError::StorageRetrieval)?;
        Ok(result.pop().unwrap())
    }

    /// Performs a runtime call using the best block, or a recent best block.
    ///
    /// The [`JsonRpcService`] maintains the code of the runtime of a recent best block locally,
    /// but doesn't know anything about the storage, which the runtime might have to access. In
    /// order to make this work, a "call proof" is performed on the network in order to obtain
    /// the storage values corresponding to this call.
    async fn recent_best_block_runtime_call(
        self: &Arc<JsonRpcService>,
        method: &str,
        parameter_vectored: impl Iterator<Item = impl AsRef<[u8]>> + Clone,
    ) -> Result<Vec<u8>, RuntimeCallError> {
        // `latest_known_runtime` should be kept locked as little as possible.
        // In order to handle the possibility a runtime upgrade happening during the operation,
        // every time `latest_known_runtime` is locked, we compare the runtime version stored in
        // it with the value previously found. If there is a mismatch, the entire runtime call
        // is restarted from scratch.
        loop {
            // Get `runtime_block_hash` and `runtime_block_state_root`, the hash and state trie
            // root of a recent best block that uses this runtime.
            let (spec_version, runtime_block_hash, runtime_block_state_root) = {
                let lock = self.latest_known_runtime.lock().await;
                (
                    lock.runtime
                        .as_ref()
                        .map_err(|()| RuntimeCallError::InvalidRuntime)?
                        .runtime_spec
                        .decode()
                        .spec_version,
                    lock.runtime_block_hash,
                    lock.runtime_block_state_root,
                )
            };

            // Perform the call proof request.
            // Note that `latest_known_runtime` is not locked.
            // If the call proof fail, do as if the proof was empty. This will enable the
            // fallback consisting in performing individual storage proof requests.
            let call_proof = self
                .network_service
                .clone()
                .call_proof_query(protocol::CallProofRequestConfig {
                    block_hash: runtime_block_hash,
                    method,
                    parameter_vectored: parameter_vectored.clone(),
                })
                .await
                .unwrap_or(Vec::new());

            // Lock `latest_known_runtime_lock` again. `continue` if the runtime has changed
            // in-between.
            let mut latest_known_runtime_lock = self.latest_known_runtime.lock().await;
            let runtime = latest_known_runtime_lock
                .runtime
                .as_mut()
                .map_err(|()| RuntimeCallError::InvalidRuntime)?;
            if runtime.runtime_spec.decode().spec_version != spec_version {
                continue;
            }

            // Perform the actual runtime call locally.
            let mut runtime_call =
                executor::read_only_runtime_host::run(executor::read_only_runtime_host::Config {
                    virtual_machine: runtime.virtual_machine.take().unwrap(),
                    function_to_call: method,
                    parameter: parameter_vectored,
                })
                .map_err(RuntimeCallError::StartError)?; // TODO: must put back virtual machine /!\

            loop {
                match runtime_call {
                    executor::read_only_runtime_host::RuntimeHostVm::Finished(Ok(success)) => {
                        if !success.logs.is_empty() {
                            log::debug!(
                                target: "json-rpc",
                                "Runtime logs: {}",
                                success.logs
                            );
                        }

                        let return_value = success.virtual_machine.value().as_ref().to_owned();
                        runtime.virtual_machine = Some(success.virtual_machine.into_prototype());
                        return Ok(return_value);
                    }
                    executor::read_only_runtime_host::RuntimeHostVm::Finished(Err(error)) => {
                        // TODO: put back virtual_machine /!\
                        return Err(RuntimeCallError::CallError(error));
                    }
                    executor::read_only_runtime_host::RuntimeHostVm::StorageGet(get) => {
                        let requested_key = get.key_as_vec(); // TODO: optimization: don't use as_vec
                        let storage_value = proof_verify::verify_proof(proof_verify::Config {
                            requested_key: &requested_key,
                            trie_root_hash: &runtime_block_state_root,
                            proof: call_proof.iter().map(|v| &v[..]),
                        })
                        .unwrap(); // TODO: shouldn't unwrap but do storage_proof instead
                        runtime_call = get.inject_value(storage_value.as_ref().map(iter::once));
                    }
                    executor::read_only_runtime_host::RuntimeHostVm::NextKey(_) => {
                        todo!() // TODO:
                    }
                }
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

#[derive(Debug, derive_more::Display)]
enum RuntimeCallError {
    /// Error during the runtime call.
    #[display(fmt = "{}", _0)]
    CallError(executor::read_only_runtime_host::Error),
    /// Error initializing the runtime call.
    #[display(fmt = "{}", _0)]
    StartError(executor::host::StartErr),
    /// Runtime of the best block isn't valid.
    #[display(fmt = "Runtime of the best block isn't valid")]
    InvalidRuntime,
    /// Error while retrieving the storage item from other nodes.
    #[display(fmt = "{}", _0)]
    StorageRetrieval(network_service::CallProofQueryError),
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
