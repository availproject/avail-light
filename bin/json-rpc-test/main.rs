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

// TODO: temporary binary to try the JSON-RPC server component alone

#![deny(broken_intra_doc_links)]

use core::convert::TryFrom as _;
use std::collections::{BTreeMap, HashMap};
use substrate_lite::json_rpc::{methods, websocket_server};

fn main() {
    futures::executor::block_on(async_main())
}

async fn async_main() {
    let chain_spec = substrate_lite::chain_spec::ChainSpec::from_json_bytes(
        &include_bytes!("../polkadot.json")[..],
    )
    .unwrap();

    let mut server = websocket_server::WsServer::new(websocket_server::Config {
        bind_address: "0.0.0.0:9944".parse().unwrap(),
        max_frame_size: 1024 * 1024,
        send_buffer_len: 32,
        capacity: 16,
    })
    .await
    .unwrap();

    let genesis_storage = chain_spec.genesis_storage().collect::<BTreeMap<_, _>>();

    let genesis_block_header =
        substrate_lite::calculate_genesis_block_header(chain_spec.genesis_storage());
    let genesis_block_hash = genesis_block_header.hash();

    let metadata = {
        let code = genesis_storage.get(&b":code"[..]).unwrap();
        let heap_pages = 1024; // TODO: laziness
        substrate_lite::metadata::metadata_from_runtime_code(code, heap_pages).unwrap()
    };

    let mut next_subscription = 0u64;
    let mut runtime_version_subscriptions = HashMap::new();
    let mut all_heads_subscriptions = HashMap::new();
    let mut new_heads_subscriptions = HashMap::new();
    let mut finalized_heads_subscriptions = HashMap::new();
    let mut storage_subscriptions = HashMap::new();

    struct Subscriptions {
        runtime_version: Vec<String>,
        all_heads: Vec<String>,
        new_heads: Vec<String>,
        finalized_heads: Vec<String>,
        storage: Vec<String>,
    }

    loop {
        let (connection_id, response1, response2) = match server.next_event().await {
            websocket_server::Event::ConnectionOpen { .. } => {
                server.accept(Subscriptions {
                    runtime_version: Vec::new(),
                    all_heads: Vec::new(),
                    new_heads: Vec::new(),
                    finalized_heads: Vec::new(),
                    storage: Vec::new(),
                });
                continue;
            }
            websocket_server::Event::ConnectionError {
                connection_id,
                user_data,
            } => {
                for runtime_version in user_data.runtime_version {
                    let _user_data = runtime_version_subscriptions.remove(&runtime_version);
                    debug_assert_eq!(_user_data, Some(connection_id));
                }
                for new_heads in user_data.new_heads {
                    let _user_data = new_heads_subscriptions.remove(&new_heads);
                    debug_assert_eq!(_user_data, Some(connection_id));
                }
                for storage in user_data.storage {
                    let _user_data = storage_subscriptions.remove(&storage);
                    debug_assert_eq!(_user_data, Some(connection_id));
                }
                continue;
            }
            websocket_server::Event::TextFrame {
                connection_id,
                message,
                user_data,
            } => {
                let (request_id, call) = methods::parse_json_call(&message).expect("bad request");
                match call {
                    methods::MethodCall::author_pendingExtrinsics {} => {
                        let response = methods::Response::author_pendingExtrinsics(Vec::new())
                            .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::chain_getBlockHash { height } => {
                        assert_eq!(height, Some(0));
                        let response = methods::Response::chain_getBlockHash(
                            methods::HashHexString(genesis_block_hash),
                        )
                        .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::chain_getFinalizedHead {} => {
                        let response = methods::Response::chain_getFinalizedHead(
                            methods::HashHexString(genesis_block_hash),
                        )
                        .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::chain_getHeader { hash } => {
                        // TODO: use hash parameter
                        let response = methods::Response::chain_getHeader(methods::Header {
                            parent_hash: methods::HashHexString(genesis_block_header.parent_hash),
                            extrinsics_root: methods::HashHexString(
                                genesis_block_header.extrinsics_root,
                            ),
                            state_root: methods::HashHexString(genesis_block_header.state_root),
                            number: genesis_block_header.number,
                            digest: methods::HeaderDigest {
                                logs: genesis_block_header
                                    .digest
                                    .logs()
                                    .map(|log| {
                                        methods::HexString(log.scale_encoding().fold(
                                            Vec::new(),
                                            |mut a, b| {
                                                a.extend_from_slice(b.as_ref());
                                                a
                                            },
                                        ))
                                    })
                                    .collect(),
                            },
                        })
                        .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::chain_subscribeAllHeads {} => {
                        let subscription = next_subscription.to_string();
                        next_subscription += 1;

                        let response = methods::Response::chain_subscribeAllHeads(&subscription)
                            .to_json_response(request_id);
                        user_data.all_heads.push(subscription.clone());
                        all_heads_subscriptions.insert(subscription, connection_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::chain_subscribeNewHeads {} => {
                        let subscription = next_subscription.to_string();
                        next_subscription += 1;

                        let response = methods::Response::chain_subscribeNewHeads(&subscription)
                            .to_json_response(request_id);
                        user_data.new_heads.push(subscription.clone());
                        new_heads_subscriptions.insert(subscription, connection_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::chain_subscribeFinalizedHeads {} => {
                        let subscription = next_subscription.to_string();
                        next_subscription += 1;

                        let response =
                            methods::Response::chain_subscribeFinalizedHeads(&subscription)
                                .to_json_response(request_id);
                        user_data.finalized_heads.push(subscription.clone());
                        finalized_heads_subscriptions.insert(subscription, connection_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::rpc_methods {} => {
                        let response = methods::Response::rpc_methods(methods::RpcMethods {
                            version: 1,
                            methods: methods::MethodCall::method_names()
                                .map(|n| n.into())
                                .collect(),
                        })
                        .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::state_queryStorageAt { keys, at } => {
                        // TODO: I have no idea what the API of this function is
                        assert!(at.is_none()); // TODO:

                        let mut out = methods::StorageChangeSet {
                            block: methods::HashHexString(genesis_block_hash),
                            changes: Vec::new(),
                        };

                        for key in keys {
                            let value = genesis_storage
                                .get(&key.0[..])
                                .map(|v| methods::HexString(v.to_vec()));
                            out.changes.push((key, value));
                        }

                        let response = methods::Response::state_queryStorageAt(vec![out])
                            .to_json_response(request_id);
                        (connection_id, response, None)
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
                        for (k, _) in genesis_storage
                            .range(start_key.as_ref().map(|p| &p.0[..]).unwrap_or(&[])..)
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
                        (connection_id, response, None)
                    }
                    methods::MethodCall::state_getMetadata {} => {
                        let response = methods::Response::state_getMetadata(methods::HexString(
                            metadata.clone(),
                        ))
                        .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::state_getStorage { key, hash } => {
                        // TODO: use hash

                        let value = genesis_storage.get(&key.0[..]).unwrap().to_vec(); // TODO: don't unwrap
                        let response =
                            methods::Response::state_getStorage(methods::HexString(value))
                                .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::state_subscribeRuntimeVersion {} => {
                        let subscription = next_subscription.to_string();
                        next_subscription += 1;

                        let response =
                            methods::Response::state_subscribeRuntimeVersion(&subscription)
                                .to_json_response(request_id);
                        user_data.runtime_version.push(subscription.clone());
                        runtime_version_subscriptions.insert(subscription, connection_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::state_subscribeStorage { list } => {
                        let subscription = next_subscription.to_string();
                        next_subscription += 1;

                        let response1 = methods::Response::state_subscribeStorage(&subscription)
                            .to_json_response(request_id);
                        user_data.storage.push(subscription.clone());
                        storage_subscriptions.insert(subscription.clone(), connection_id);

                        // TODO: have no idea what this describes actually
                        let mut out = methods::StorageChangeSet {
                            block: methods::HashHexString(genesis_block_hash),
                            changes: Vec::new(),
                        };

                        for key in list {
                            let value = genesis_storage
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

                        (connection_id, response1, Some(response2))
                    }
                    methods::MethodCall::state_unsubscribeStorage { subscription } => {
                        let valid = storage_subscriptions
                            .get(&subscription)
                            .map_or(false, |id| *id == connection_id);
                        if valid {
                            storage_subscriptions.remove(&subscription);
                            user_data.storage.retain(|s| *s != subscription);
                        };

                        let response = methods::Response::state_unsubscribeStorage(valid)
                            .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::state_getRuntimeVersion {} => {
                        // FIXME: hack
                        let response =
                            methods::Response::state_getRuntimeVersion(methods::RuntimeVersion {
                                spec_name: "polkadot".to_string(),
                                impl_name: "substrate-lite".to_string(),
                                authoring_version: 0,
                                spec_version: 23,
                                impl_version: 0,
                                transaction_version: 4,
                            })
                            .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::system_chain {} => {
                        let response = methods::Response::system_chain(chain_spec.name())
                            .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::system_chainType {} => {
                        let response = methods::Response::system_chainType(chain_spec.chain_type())
                            .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::system_health {} => {
                        let response = methods::Response::system_health(methods::SystemHealth {
                            is_syncing: true,        // TODO:
                            peers: 1,                // TODO:
                            should_have_peers: true, // TODO:
                        })
                        .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::system_name {} => {
                        let response = methods::Response::system_name("substrate-lite!")
                            .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::system_properties {} => {
                        let response = methods::Response::system_properties(
                            serde_json::from_str(chain_spec.properties()).unwrap(),
                        )
                        .to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    methods::MethodCall::system_version {} => {
                        let response =
                            methods::Response::system_version("1.0.0").to_json_response(request_id);
                        (connection_id, response, None)
                    }
                    _ => {
                        println!("unimplemented: {:?}", call);
                        continue;
                    }
                }
            }
        };

        server.queue_send(connection_id, response1);
        if let Some(response2) = response2 {
            server.queue_send(connection_id, response2);
        }
    }
}
