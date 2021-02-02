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

use futures::prelude::*;
use smoldot::{
    chain_spec, executor, header,
    json_rpc::{self, methods},
};
use std::{
    collections::{BTreeMap, HashSet},
    convert::TryFrom as _,
    pin::Pin,
    sync::Arc,
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
pub async fn start(mut config: Config) {
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

    let (finalized_block_header, mut finalized_blocks_subscription) =
        config.sync_service.subscribe_finalized().await;
    let (_, mut best_blocks_subscription) = config.sync_service.subscribe_best().await;

    let finalized_block_hash = header::hash_from_scale_encoded_header(&finalized_block_header);

    let mut known_blocks = lru::LruCache::new(256);
    known_blocks.put(
        finalized_block_hash,
        header::decode(&finalized_block_header).unwrap().into(),
    );

    let mut client = JsonRpcService {
        chain_spec: config.chain_spec,
        network_service: config.network_service,
        sync_service: config.sync_service,
        known_blocks,
        best_block: finalized_block_hash,
        finalized_block: finalized_block_hash,
        genesis_block: config.genesis_block_hash,
        genesis_storage,
        best_block_metadata,
        best_block_runtime_spec,
        next_subscription: 0,
        runtime_version: HashSet::new(),
        all_heads: HashSet::new(),
        new_heads: HashSet::new(),
        finalized_heads: HashSet::new(),
        storage: HashSet::new(),
    };

    (config.tasks_executor)(Box::pin(async move {
        loop {
            futures::select! {
                json_rpc_request = ffi::next_json_rpc().fuse() => {
                    // TODO: don't unwrap
                    let request_string = String::from_utf8(Vec::from(json_rpc_request)).unwrap();
                    log::debug!(
                        target: "json-rpc",
                        "JSON-RPC => {:?}{}",
                        if request_string.len() > 100 { &request_string[..100] } else { &request_string[..] },
                        if request_string.len() > 100 { "…" } else { "" }
                    );

                    // TODO: don't await here; use a queue
                    let (response1, response2) = handle_rpc(&request_string, &mut client).await;
                    log::debug!(
                        target: "json-rpc",
                        "JSON-RPC <= {:?}{}",
                        if response1.len() > 100 { &response1[..100] } else { &response1[..] },
                        if response1.len() > 100 { "…" } else { "" }
                    );
                    ffi::emit_json_rpc_response(&response1);

                    if let Some(response2) = response2 {
                        log::debug!(
                            target: "json-rpc",
                            "JSON-RPC <= {:?}{}",
                            if response2.len() > 100 { &response2[..100] } else { &response2[..] },
                            if response2.len() > 100 { "…" } else { "" }
                        );
                        ffi::emit_json_rpc_response(&response2);
                    }
                }
                scale_encoded_header = best_blocks_subscription.select_next_some() => {
                    // TODO: this is also triggered if we reset the sync to a previous point, which isn't correct

                    let decoded = smoldot::header::decode(&scale_encoded_header).unwrap();
                    let header = header_conv(decoded.clone());

                    for subscription_id in &client.new_heads {
                        let notification = smoldot::json_rpc::parse::build_subscription_event(
                            "chain_newHead",
                            subscription_id,
                            &serde_json::to_string(&header).unwrap(),
                        );
                        ffi::emit_json_rpc_response(&notification);
                    }
                    for subscription_id in &client.all_heads {
                        let notification = smoldot::json_rpc::parse::build_subscription_event(
                            "chain_newHead",
                            subscription_id,
                            &serde_json::to_string(&header).unwrap(),
                        );
                        ffi::emit_json_rpc_response(&notification);
                    }

                    // Load the entry of the finalized block in order to guarantee that it
                    // remains in the LRU cache when `put` is called below.
                    let _ = client.known_blocks.get(&client.finalized_block).unwrap();

                    client.best_block = decoded.hash();
                    client.known_blocks.put(client.best_block, decoded.into());

                    debug_assert!(client.known_blocks.get(&client.finalized_block).is_some());

                    // TODO: need to update `best_block_metadata` if necessary, and notify the runtime version subscriptions
                },
                scale_encoded_header = finalized_blocks_subscription.select_next_some() => {
                    let decoded = smoldot::header::decode(&scale_encoded_header).unwrap();
                    let header = header_conv(decoded.clone());

                    for subscription_id in &client.finalized_heads {
                        let notification = smoldot::json_rpc::parse::build_subscription_event(
                            "chain_finalizedHead",
                            subscription_id,
                            &serde_json::to_string(&header).unwrap(),
                        );
                        ffi::emit_json_rpc_response(&notification);
                    }

                    client.finalized_block = decoded.hash();
                    client.known_blocks.put(client.finalized_block, decoded.into());
                },
            }
        }
    }));
}

struct JsonRpcService {
    chain_spec: chain_spec::ChainSpec,

    network_service: Arc<network_service::NetworkService>,
    sync_service: Arc<sync_service::SyncService>,

    /// Blocks that are temporarily saved in order to serve JSON-RPC requests.
    ///
    /// Always contains `best_block` and `finalized_block`.
    known_blocks: lru::LruCache<[u8; 32], header::Header>,

    /// Hash of the genesis block.
    /// Keeping the genesis block is important, as the genesis block hash is included in
    /// transaction signatures, and must therefore be queried by upper-level UIs.
    genesis_block: [u8; 32],
    /// Hash of the current best block.
    best_block: [u8; 32],
    /// Hash of the latest finalized block.
    finalized_block: [u8; 32],

    // TODO: remove; unnecessary
    genesis_storage: BTreeMap<Vec<u8>, Vec<u8>>,

    best_block_metadata: Vec<u8>,
    best_block_runtime_spec: executor::CoreVersion,

    next_subscription: u64,

    runtime_version: HashSet<String>,
    all_heads: HashSet<String>,
    new_heads: HashSet<String>,
    finalized_heads: HashSet<String>,
    storage: HashSet<String>,
}

async fn handle_rpc(rpc: &str, client: &mut JsonRpcService) -> (String, Option<String>) {
    let (request_id, call) = methods::parse_json_call(rpc).expect("bad request"); // TODO: don't unwrap
    match call {
        methods::MethodCall::author_pendingExtrinsics {} => {
            let response = methods::Response::author_pendingExtrinsics(Vec::new())
                .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::author_submitExtrinsic { transaction } => {
            let response = match client
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
            (response, None)
        }
        methods::MethodCall::chain_getBlockHash { height } => {
            let response = match height {
                Some(0) => methods::Response::chain_getBlockHash(methods::HashHexString(
                    client.genesis_block,
                ))
                .to_json_response(request_id),
                None => {
                    methods::Response::chain_getBlockHash(methods::HashHexString(client.best_block))
                        .to_json_response(request_id)
                }
                Some(n)
                    if client
                        .known_blocks
                        .get(&client.best_block)
                        .map_or(false, |h| h.number == n) =>
                {
                    methods::Response::chain_getBlockHash(methods::HashHexString(client.best_block))
                        .to_json_response(request_id)
                }
                Some(n)
                    if client
                        .known_blocks
                        .get(&client.finalized_block)
                        .map_or(false, |h| h.number == n) =>
                {
                    methods::Response::chain_getBlockHash(methods::HashHexString(
                        client.finalized_block,
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

            (response, None)
        }
        methods::MethodCall::chain_getFinalizedHead {} => {
            let response = methods::Response::chain_getFinalizedHead(methods::HashHexString(
                client.finalized_block,
            ))
            .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::chain_getHeader { hash } => {
            let hash = hash.as_ref().map(|h| &h.0).unwrap_or(&client.best_block);
            let response = if let Some(header) = client.known_blocks.get(hash) {
                methods::Response::chain_getHeader(header_conv(header)).to_json_response(request_id)
            } else {
                json_rpc::parse::build_success_response(request_id, "null")
            };

            (response, None)
        }
        methods::MethodCall::chain_subscribeAllHeads {} => {
            let subscription = client.next_subscription.to_string();
            client.next_subscription += 1;

            let response = methods::Response::chain_subscribeAllHeads(&subscription)
                .to_json_response(request_id);

            // TODO: directly use sync_service
            let response2 = smoldot::json_rpc::parse::build_subscription_event(
                "chain_allHeads", // TODO: is this string correct?
                &subscription,
                &serde_json::to_string(&header_conv(
                    client.known_blocks.get(&client.best_block).unwrap(),
                ))
                .unwrap(),
            );

            client.all_heads.insert(subscription.clone());

            (response, Some(response2))
        }
        methods::MethodCall::chain_subscribeNewHeads {} => {
            let subscription = client.next_subscription.to_string();
            client.next_subscription += 1;

            let response = methods::Response::chain_subscribeNewHeads(&subscription)
                .to_json_response(request_id);

            let response2 = smoldot::json_rpc::parse::build_subscription_event(
                "chain_newHead",
                &subscription,
                &serde_json::to_string(&header_conv(
                    client.known_blocks.get(&client.best_block).unwrap(),
                ))
                .unwrap(),
            );

            client.new_heads.insert(subscription.clone());

            (response, Some(response2))
        }
        methods::MethodCall::chain_subscribeFinalizedHeads {} => {
            let subscription = client.next_subscription.to_string();
            client.next_subscription += 1;

            let response = methods::Response::chain_subscribeFinalizedHeads(&subscription)
                .to_json_response(request_id);

            // TODO: directly use sync_service
            let response2 = smoldot::json_rpc::parse::build_subscription_event(
                "chain_finalizedHead",
                &subscription,
                &serde_json::to_string(&header_conv(
                    client.known_blocks.get(&client.finalized_block).unwrap(),
                ))
                .unwrap(),
            );

            client.finalized_heads.insert(subscription.clone());

            (response, Some(response2))
        }
        methods::MethodCall::chain_unsubscribeFinalizedHeads { subscription } => {
            let valid = client.finalized_heads.remove(&subscription);
            let response = methods::Response::chain_unsubscribeFinalizedHeads(valid)
                .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::rpc_methods {} => {
            let response = methods::Response::rpc_methods(methods::RpcMethods {
                version: 1,
                methods: methods::MethodCall::method_names()
                    .map(|n| n.into())
                    .collect(),
            })
            .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::state_queryStorageAt { keys, at } => {
            let at = at.as_ref().map(|h| h.0).unwrap_or(client.best_block);

            // TODO: have no idea what this describes actually
            let mut out = methods::StorageChangeSet {
                block: methods::HashHexString(client.best_block),
                changes: Vec::new(),
            };

            for key in keys {
                // TODO: parallelism?
                if let Ok(value) = storage_query(client, &key.0, &at).await {
                    out.changes.push((key, value.map(methods::HexString)));
                }
            }

            let response =
                methods::Response::state_queryStorageAt(vec![out]).to_json_response(request_id);
            (response, None)
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
            for (k, _) in client
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

            let response = methods::Response::state_getKeysPaged(out).to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::state_getMetadata {} => {
            let best_block_hash = client.best_block.clone();
            let response = match storage_query(client, &b":code"[..], &best_block_hash).await {
                Ok(Some(value)) => {
                    let best_block_metadata = {
                        let heap_pages = executor::DEFAULT_HEAP_PAGES; // TODO: laziness
                        smoldot::metadata::metadata_from_runtime_code(&value, heap_pages).unwrap()
                    };

                    client.best_block_metadata = best_block_metadata;

                    methods::Response::state_getMetadata(methods::HexString(
                        client.best_block_metadata.clone(),
                    ))
                    .to_json_response(request_id)
                }
                Ok(None) => json_rpc::parse::build_success_response(request_id, "null"),
                Err(_) => {
                    // Return the last known best_block_metadata
                    methods::Response::state_getMetadata(methods::HexString(
                        client.best_block_metadata.clone(),
                    ))
                    .to_json_response(request_id)
                }
            };

            (response, None)
        }
        methods::MethodCall::state_getStorage { key, hash } => {
            let hash = hash.as_ref().map(|h| h.0).unwrap_or(client.best_block);

            let response = match storage_query(client, &key.0, &hash).await {
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

            (response, None)
        }
        methods::MethodCall::state_subscribeRuntimeVersion {} => {
            let subscription = client.next_subscription.to_string();
            client.next_subscription += 1;

            let response = methods::Response::state_subscribeRuntimeVersion(&subscription)
                .to_json_response(request_id);
            // TODO: subscription has no effect right now
            client.runtime_version.insert(subscription.clone());

            let best_block_hash = client.best_block;
            let runtime_code = storage_query(client, &b":code"[..], &best_block_hash).await;
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
                client.best_block_runtime_spec.clone()
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
            (response, Some(response2))
        }
        methods::MethodCall::state_subscribeStorage { list } => {
            let subscription = client.next_subscription.to_string();
            client.next_subscription += 1;

            let response1 = methods::Response::state_subscribeStorage(&subscription)
                .to_json_response(request_id);
            client.storage.insert(subscription.clone());

            // TODO: have no idea what this describes actually
            let mut out = methods::StorageChangeSet {
                block: methods::HashHexString(client.best_block),
                changes: Vec::new(),
            };

            let best_block_hash = client.best_block;

            for key in list {
                // TODO: parallelism?
                if let Ok(value) = storage_query(client, &key.0, &best_block_hash).await {
                    out.changes.push((key, value.map(methods::HexString)));
                }
            }

            // TODO: hack
            let response2 = smoldot::json_rpc::parse::build_subscription_event(
                "state_storage",
                &subscription,
                &serde_json::to_string(&out).unwrap(),
            );

            // TODO: subscription not actually implemented

            (response1, Some(response2))
        }
        methods::MethodCall::state_unsubscribeStorage { subscription } => {
            let valid = client.storage.remove(&subscription);
            let response =
                methods::Response::state_unsubscribeStorage(valid).to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::state_getRuntimeVersion {} => {
            let best_block_hash = client.best_block;
            let runtime_code = storage_query(client, &b":code"[..], &best_block_hash).await;
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
                client.best_block_runtime_spec.clone()
            };

            let runtime_specs = runtime_specs.decode();
            let response = methods::Response::state_getRuntimeVersion(methods::RuntimeVersion {
                spec_name: runtime_specs.spec_name.into(),
                impl_name: runtime_specs.impl_name.into(),
                authoring_version: u64::from(runtime_specs.authoring_version),
                spec_version: u64::from(runtime_specs.spec_version),
                impl_version: u64::from(runtime_specs.impl_version),
                transaction_version: runtime_specs.transaction_version.map(u64::from),
                apis: runtime_specs.apis,
            })
            .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::system_chain {} => {
            let response = methods::Response::system_chain(client.chain_spec.name())
                .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::system_chainType {} => {
            let response = methods::Response::system_chainType(client.chain_spec.chain_type())
                .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::system_health {} => {
            let response = methods::Response::system_health(methods::SystemHealth {
                is_syncing: !client.sync_service.is_above_network_finalized().await,
                peers: u64::try_from(client.network_service.peers_list().await.count())
                    .unwrap_or(u64::max_value()),
                should_have_peers: client.chain_spec.has_live_network(),
            })
            .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::system_name {} => {
            let response = methods::Response::system_name("smoldot!").to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::system_peers {} => {
            // TODO: return proper response
            let response = methods::Response::system_peers(
                client
                    .network_service
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
            (response, None)
        }
        methods::MethodCall::system_properties {} => {
            let response = methods::Response::system_properties(
                serde_json::from_str(client.chain_spec.properties()).unwrap(),
            )
            .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::system_version {} => {
            let response = methods::Response::system_version("1.0.0").to_json_response(request_id);
            (response, None)
        }
        _ => {
            println!("unimplemented: {:?}", call);
            panic!(); // TODO:
        }
    }
}

async fn storage_query(
    client: &mut JsonRpcService,
    key: &[u8],
    hash: &[u8; 32],
) -> Result<Option<Vec<u8>>, StorageQueryError> {
    let trie_root_hash = if let Some(header) = client.known_blocks.get(hash) {
        header.state_root
    } else {
        // TODO: should make a block request towards a node
        return Err(StorageQueryError::FindStorageRootHashError);
    };

    client
        .network_service
        .clone()
        .storage_query(hash, &trie_root_hash, key)
        .await
        .map_err(StorageQueryError::StorageRetrieval)
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
