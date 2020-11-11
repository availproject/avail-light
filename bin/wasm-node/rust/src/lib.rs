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
use std::{collections::HashMap, num::NonZeroU32, sync::Arc};
use substrate_lite::{
    chain,
    chain::sync::headers_optimistic,
    chain_spec, json_rpc,
    network::{self, multiaddr, peer_id::PeerId, protocol},
};

pub mod ffi;

mod network_service;

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

    let (mut to_sync_tx, to_sync_rx) = mpsc::channel(64);
    let (to_db_save_tx, _to_db_save_rx) = mpsc::channel(16);

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

    ffi::spawn_task(
        start_sync(
            chain_information,
            network_service.clone(),
            to_sync_rx,
            to_db_save_tx,
        )
        .await,
    );

    loop {
        futures::select! {
            network_message = network_service.next_event().fuse() => {
                match network_message {
                    network_service::Event::Connected(peer_id) => {
                        to_sync_tx.send(ToSync::NewPeer(peer_id)).await.unwrap();
                    }
                    network_service::Event::Disconnected(peer_id) => {
                        to_sync_tx.send(ToSync::PeerDisconnected(peer_id)).await.unwrap();
                    }
                }
            },

            json_rpc_request = ffi::next_json_rpc().fuse() => {
                // TODO: don't unwrap
                handle_rpc(&String::from_utf8(Vec::from(json_rpc_request)).unwrap(), &chain_spec).await;
            }
        }
    }
}

async fn handle_rpc(rpc: &str, chain_spec: &chain_spec::ChainSpec) {
    let (request_id, call) = json_rpc::methods::parse_json_call(rpc).unwrap(); // TODO: don't unwrap

    let response = match call {
        json_rpc::methods::MethodCall::system_chain {} => {
            let value = json_rpc::methods::Response::system_chain(chain_spec.name())
                .to_json_response(request_id);
            async move { value }.boxed()
        }
        json_rpc::methods::MethodCall::system_chainType {} => {
            let value = json_rpc::methods::Response::system_chainType(chain_spec.chain_type())
                .to_json_response(request_id);
            async move { value }.boxed()
        }
        json_rpc::methods::MethodCall::system_name {} => {
            let value = json_rpc::methods::Response::system_name("Polkadot ✨ lite ✨")
                .to_json_response(request_id);
            async move { value }.boxed()
        }
        json_rpc::methods::MethodCall::system_version {} => {
            let value =
                json_rpc::methods::Response::system_version("??").to_json_response(request_id);
            async move { value }.boxed()
        }
        // TODO: implement the rest
        _ => todo!("not implemented: {:?}", call),
    };

    ffi::emit_json_rpc_response(&response.await);
}

async fn start_sync(
    chain_information: chain::chain_information::ChainInformation,
    network_service: Arc<network_service::NetworkService>,
    mut to_sync: mpsc::Receiver<ToSync>,
    mut to_db_save_tx: mpsc::Sender<chain::chain_information::ChainInformation>,
) -> impl Future<Output = ()> {
    let mut sync = headers_optimistic::OptimisticHeadersSync::<_, network::PeerId>::new(
        headers_optimistic::Config {
            chain_information,
            sources_capacity: 32,
            source_selection_randomness_seed: rand::random(),
            blocks_request_granularity: NonZeroU32::new(128).unwrap(),
            download_ahead_blocks: {
                // Assuming a verification speed of 1k blocks/sec and a 95% latency of one second,
                // the number of blocks to download ahead of time in order to not block is 1000.
                1024
            },
        },
    );

    async move {
        let mut peers_source_id_map = HashMap::new();
        let mut block_requests_finished = stream::FuturesUnordered::new();

        loop {
            while let Some(action) = sync.next_request_action() {
                match action {
                    headers_optimistic::RequestAction::Start {
                        start,
                        block_height,
                        source,
                        num_blocks,
                        ..
                    } => {
                        let block_request = network_service.clone().blocks_request(
                            source.clone(),
                            protocol::BlocksRequestConfig {
                                start: protocol::BlocksRequestConfigStart::Number(block_height),
                                desired_count: num_blocks,
                                direction: protocol::BlocksRequestDirection::Ascending,
                                fields: protocol::BlocksRequestFields {
                                    header: true,
                                    body: false,
                                    justification: true,
                                },
                            },
                        );

                        let (block_request, abort) = future::abortable(block_request);
                        let request_id = start.start(abort);
                        block_requests_finished.push(block_request.map(move |r| (request_id, r)));
                    }
                    headers_optimistic::RequestAction::Cancel { user_data, .. } => {
                        user_data.abort();
                    }
                }
            }

            // Verify blocks that have been fetched from queries.
            loop {
                match sync.process_one(ffi::unix_time()) {
                    headers_optimistic::ProcessOneOutcome::Idle => break,
                    headers_optimistic::ProcessOneOutcome::Updated {
                        best_block_hash,
                        best_block_number,
                        ..
                    } => {}
                    headers_optimistic::ProcessOneOutcome::Reset {
                        reason,
                        new_best_block_hash,
                        new_best_block_number,
                    } => {}
                }

                // Since `process_one` is a CPU-heavy operation, looping until it is done can
                // take a long time. In order to avoid blocking the rest of the program in the
                // meanwhile, the `yield_once` function interrupts the current task and gives a
                // chance for other tasks to progress.
                yield_once().await;
            }

            // TODO: save less often
            let _ = to_db_save_tx.send(sync.as_chain_information().into()).await;

            futures::select! {
                message = to_sync.next() => {
                    let message = match message {
                        Some(m) => m,
                        None => {
                            return
                        },
                    };

                    match message {
                        ToSync::NewPeer(peer_id) => {
                            let id = sync.add_source(peer_id.clone());
                            peers_source_id_map.insert(peer_id.clone(), id);
                        },
                        ToSync::PeerDisconnected(peer_id) => {
                            let id = peers_source_id_map.remove(&peer_id).unwrap();
                            let (_, rq_list) = sync.remove_source(id);
                            for (_, rq) in rq_list {
                                rq.abort();
                            }
                        },
                    }
                },

                (request_id, result) = block_requests_finished.select_next_some() => {
                    // `result` is an error if the block request got cancelled by the sync state
                    // machine.
                    if let Ok(result) = result {
                        let _ = sync.finish_request(request_id, result.map(|v| v.into_iter().map(|block| headers_optimistic::RequestSuccessBlock {
                            scale_encoded_header: block.header.unwrap(), // TODO: don't unwrap
                            scale_encoded_justification: block.justification,
                        })).map_err(|()| headers_optimistic::RequestFail::BlocksUnavailable));
                    }
                },
            }
        }
    }
}

enum ToSync {
    NewPeer(network::PeerId),
    PeerDisconnected(network::PeerId),
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
