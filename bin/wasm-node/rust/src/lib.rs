// Substrate-lite
// Copyright (C) 2019-2020  Parity Technologies (UK) Ltd.
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

//! Contains a light client implementation usable from a browser environment, using the
//! `wasm-bindgen` library.

#![recursion_limit = "512"]

use futures::{channel::mpsc, prelude::*};
use std::{
    collections::{BTreeMap, HashSet},
    convert::TryFrom as _,
    sync::Arc,
};
use substrate_lite::{
    chain, chain_spec,
    json_rpc::methods,
    network::{multiaddr, peer_id::PeerId},
};

pub mod ffi;

mod network_service;
mod sync_service;

// This custom allocator is used in order to reduce the size of the Wasm binary.
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

// TODO: several places in this module where we unwrap when we shouldn't

/// Starts a client running the given chain specifications.
///
/// > **Note**: This function returns a `Result`. The return value according to the JavaScript
/// >           function is what is in the `Ok`. If an `Err` is returned, a JavaScript exception
/// >           is thrown.
pub async fn start_client(chain_spec: String) {
    std::panic::set_hook(Box::new(|info| {
        ffi::throw(info.to_string());
    }));

    // Fool-proof check to make sure that randomness is properly implemented.
    assert_ne!(rand::random::<u64>(), 0);
    assert_ne!(rand::random::<u64>(), rand::random::<u64>());

    let chain_spec = match chain_spec::ChainSpec::from_json_bytes(&chain_spec) {
        Ok(cs) => cs,
        Err(err) => ffi::throw(format!("Error while opening chain specs: {}", err)),
    };

    // Load the information about the chain from the chain specs. If a light sync state is
    // present in the chain specs, it is possible to start sync at the finalized block it
    // describes.
    let chain_information = if let Some(light_sync_state) = chain_spec.light_sync_state() {
        light_sync_state.as_chain_information()
    } else {
        chain::chain_information::ChainInformation::from_genesis_storage(
            chain_spec.genesis_storage(),
        )
        .unwrap()
    };

    // TODO: un-Arc-ify
    let network_service = network_service::NetworkService::new(network_service::Config {
        tasks_executor: Box::new(|fut| ffi::spawn_task(fut)),
        bootstrap_nodes: {
            let mut list = Vec::with_capacity(chain_spec.boot_nodes().len());
            for node in chain_spec.boot_nodes() {
                let mut address: multiaddr::Multiaddr = node.parse().unwrap(); // TODO: don't unwrap?
                if let Some(multiaddr::Protocol::P2p(peer_id)) = address.pop() {
                    let peer_id = PeerId::from_multihash(peer_id).unwrap(); // TODO: don't unwrap
                    list.push((peer_id, address));
                } else {
                    panic!() // TODO:
                }
            }
            list
        },
        protocol_id: chain_spec.protocol_id().to_string(),
    })
    .await
    .unwrap();

    let sync_service = Arc::new(
        sync_service::SyncService::new(sync_service::Config {
            chain_information,
            tasks_executor: Box::new(|fut| ffi::spawn_task(fut)),
        })
        .await,
    );

    let genesis_storage = chain_spec
        .genesis_storage()
        .map(|(k, v)| (k.to_vec(), v.to_vec()))
        .collect::<BTreeMap<_, _>>();

    let genesis_block_header =
        substrate_lite::calculate_genesis_block_header(chain_spec.genesis_storage());
    let genesis_block_hash = genesis_block_header.hash();

    let metadata = {
        let code = genesis_storage.get(&b":code"[..]).unwrap();
        let heap_pages = 1024; // TODO: laziness
        substrate_lite::metadata::metadata_from_runtime_code(code, heap_pages).unwrap()
    };

    let mut client = Client {
        chain_spec,
        genesis_storage,
        genesis_block_hash,
        genesis_block_header,
        metadata,
        next_subscription: 0,
        runtime_version: HashSet::new(),
        all_heads: HashSet::new(),
        new_heads: HashSet::new(),
        finalized_heads: HashSet::new(),
        storage: HashSet::new(),
    };

    loop {
        futures::select! {
            network_message = network_service.next_event().fuse() => {
                match network_message {
                    network_service::Event::Connected(peer_id) => {
                        sync_service.add_source(peer_id).await;
                    }
                    network_service::Event::Disconnected(peer_id) => {
                        sync_service.remove_source(peer_id).await;
                    }
                }
            },

            sync_message = sync_service.next_event().fuse() => {
                match sync_message {
                    sync_service::Event::BlocksRequest { id, target, request } => {
                        let block_request = network_service.clone().blocks_request(
                            target,
                            request
                        );

                        ffi::spawn_task({
                            let sync_service = sync_service.clone();
                            async move {
                                let result = block_request.await;
                                sync_service.answer_blocks_request(id, result).await;
                            }
                        });
                    },
                    sync_service::Event::NewBest { scale_encoded_header } => {
                        // TODO: this is also triggered if we reset the sync to a previous point, which isn't correct

                        let decoded = substrate_lite::header::decode(&scale_encoded_header).unwrap();

                        let header = methods::Header {
                            parent_hash: methods::HashHexString(*decoded.parent_hash),
                            extrinsics_root: methods::HashHexString(
                                *decoded.extrinsics_root,
                            ),
                            state_root: methods::HashHexString(*decoded.state_root),
                            number: decoded.number,
                            digest: methods::HeaderDigest {
                                logs: decoded
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
                        };

                        for subscription_id in &client.new_heads {
                            let notification = substrate_lite::json_rpc::parse::build_subscription_event(
                                "chain_subscribeNewHeads",
                                subscription_id,
                                &serde_json::to_string(&header).unwrap(),
                            );
                            ffi::emit_json_rpc_response(&notification);
                        }
                        for subscription_id in &client.all_heads {
                            let notification = substrate_lite::json_rpc::parse::build_subscription_event(
                                "chain_subscribeAllHeads",
                                subscription_id,
                                &serde_json::to_string(&header).unwrap(),
                            );
                            ffi::emit_json_rpc_response(&notification);
                        }
                    },
                }
            },

            json_rpc_request = ffi::next_json_rpc().fuse() => {
                // TODO: don't unwrap
                let (response1, response2) = handle_rpc(&String::from_utf8(Vec::from(json_rpc_request)).unwrap(), &mut client).await;
                ffi::emit_json_rpc_response(&response1);
                if let Some(response2) = response2 {
                    ffi::emit_json_rpc_response(&response2);
                }
            },
        }
    }
}

struct Client {
    chain_spec: chain_spec::ChainSpec,

    genesis_storage: BTreeMap<Vec<u8>, Vec<u8>>,
    genesis_block_hash: [u8; 32],
    genesis_block_header: substrate_lite::header::Header,

    metadata: Vec<u8>,

    next_subscription: u64,

    runtime_version: HashSet<String>,
    all_heads: HashSet<String>,
    new_heads: HashSet<String>,
    finalized_heads: HashSet<String>,
    storage: HashSet<String>,
}

async fn handle_rpc(rpc: &str, client: &mut Client) -> (String, Option<String>) {
    let (request_id, call) = methods::parse_json_call(rpc).expect("bad request"); // TODO: don't unwrap
    match call {
        methods::MethodCall::author_pendingExtrinsics {} => {
            let response = methods::Response::author_pendingExtrinsics(Vec::new())
                .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::chain_getBlockHash { height } => {
            assert_eq!(height, 0);
            let response = methods::Response::chain_getBlockHash(methods::HashHexString(
                client.genesis_block_hash,
            ))
            .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::chain_getFinalizedHead {} => {
            let response = methods::Response::chain_getFinalizedHead(methods::HashHexString(
                client.genesis_block_hash,
            ))
            .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::chain_getHeader { hash } => {
            // TODO: use hash parameter
            let response = methods::Response::chain_getHeader(methods::Header {
                parent_hash: methods::HashHexString(client.genesis_block_header.parent_hash),
                extrinsics_root: methods::HashHexString(
                    client.genesis_block_header.extrinsics_root,
                ),
                state_root: methods::HashHexString(client.genesis_block_header.state_root),
                number: client.genesis_block_header.number,
                digest: methods::HeaderDigest {
                    logs: client
                        .genesis_block_header
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
            })
            .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::chain_subscribeAllHeads {} => {
            let subscription = client.next_subscription.to_string();
            client.next_subscription += 1;

            let response = methods::Response::chain_subscribeAllHeads(&subscription)
                .to_json_response(request_id);
            client.all_heads.insert(subscription.clone());
            // TODO: should return the current state, I think
            (response, None)
        }
        methods::MethodCall::chain_subscribeNewHeads {} => {
            let subscription = client.next_subscription.to_string();
            client.next_subscription += 1;

            let response = methods::Response::chain_subscribeNewHeads(&subscription)
                .to_json_response(request_id);
            client.new_heads.insert(subscription.clone());
            // TODO: should return the current state, I think
            (response, None)
        }
        methods::MethodCall::chain_subscribeFinalizedHeads {} => {
            let subscription = client.next_subscription.to_string();
            client.next_subscription += 1;

            let response = methods::Response::chain_subscribeFinalizedHeads(&subscription)
                .to_json_response(request_id);
            client.finalized_heads.insert(subscription.clone());
            // TODO: should return the current state, I think
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
            // TODO: I have no idea what the API of this function is
            assert!(at.is_none()); // TODO:

            let mut out = methods::StorageChangeSet {
                block: methods::HashHexString(client.genesis_block_hash),
                changes: Vec::new(),
            };

            for key in keys {
                let value = client
                    .genesis_storage
                    .get(&key.0[..])
                    .map(|v| methods::HexString(v.to_vec()));
                out.changes.push((key, value));
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
            let response =
                methods::Response::state_getMetadata(methods::HexString(client.metadata.clone()))
                    .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::state_getStorage { key, hash } => {
            // TODO: use hash

            let value = client.genesis_storage.get(&key.0[..]).unwrap().to_vec(); // TODO: don't unwrap
            let response = methods::Response::state_getStorage(methods::HexString(value))
                .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::state_subscribeRuntimeVersion {} => {
            let subscription = client.next_subscription.to_string();
            client.next_subscription += 1;

            let response = methods::Response::state_subscribeRuntimeVersion(&subscription)
                .to_json_response(request_id);
            client.runtime_version.insert(subscription.clone());
            // TODO: should return the current state, I think
            (response, None)
        }
        methods::MethodCall::state_subscribeStorage { list } => {
            let subscription = client.next_subscription.to_string();
            client.next_subscription += 1;

            let response1 = methods::Response::state_subscribeStorage(&subscription)
                .to_json_response(request_id);
            client.storage.insert(subscription.clone());

            // TODO: have no idea what this describes actually
            let mut out = methods::StorageChangeSet {
                block: methods::HashHexString(client.genesis_block_hash),
                changes: Vec::new(),
            };

            for key in list {
                let value = client
                    .genesis_storage
                    .get(&key.0[..])
                    .map(|v| methods::HexString(v.to_vec()));
                out.changes.push((key, value));
            }

            // TODO: hack
            let response2 = substrate_lite::json_rpc::parse::build_subscription_event(
                "state_storage",
                &subscription,
                &serde_json::to_string(&out).unwrap(),
            );

            (response1, Some(response2))
        }
        methods::MethodCall::state_unsubscribeStorage { subscription } => {
            let valid = client.storage.remove(&subscription);
            let response =
                methods::Response::state_unsubscribeStorage(valid).to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::state_getRuntimeVersion {} => {
            // FIXME: hack
            let response = methods::Response::state_getRuntimeVersion(methods::RuntimeVersion {
                spec_name: "polkadot".to_string(),
                impl_name: "substrate-lite".to_string(),
                authoring_version: 0,
                spec_version: 23,
                impl_version: 0,
                transaction_version: 4,
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
                is_syncing: true,        // TODO:
                peers: 1,                // TODO:
                should_have_peers: true, // TODO:
            })
            .to_json_response(request_id);
            (response, None)
        }
        methods::MethodCall::system_name {} => {
            let response =
                methods::Response::system_name("substrate-lite!").to_json_response(request_id);
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

/// Use in an asynchronous context to interrupt the current task execution and schedule it back.
///
/// This function is useful in order to guarantee a fine granularity of tasks execution time in
/// situations where a CPU-heavy task is being performed.
async fn yield_once() {
    let mut pending = true;
    futures::future::poll_fn(move |cx| {
        if pending {
            pending = false;
            cx.waker().wake_by_ref();
            core::task::Poll::Pending
        } else {
            core::task::Poll::Ready(())
        }
    })
    .await
}
