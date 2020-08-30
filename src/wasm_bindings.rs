//! Contains `wasm-bindgen` bindings.
//!
//! When this library is compiled for `wasm`, this library contains the types and functions that
//! can be accessed from the user through the `wasm-bindgen` library.

#![cfg(feature = "wasm-bindings")]
#![cfg_attr(docsrs, doc(cfg(feature = "wasm-bindings")))]

use crate::{
    chain, chain::sync::headers_optimistic, chain_spec, database, finality::grandpa, header,
    json_rpc, network, verify::babe,
};

use core::num::{NonZeroU32, NonZeroU64};
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use libp2p::wasm_ext::{ffi, ExtTransport};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct BrowserLightClient {}

// TODO: several places in this module where we unwrap when we shouldn't

#[wasm_bindgen]
pub async fn start_client(chain_spec: String) -> Result<BrowserLightClient, JsValue> {
    // TODO: don't put that here, it's a global setting that doesn't have its place in a library
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));

    // TODO: this entire function is just some temporary code before we figure out where to put it
    let chain_spec = match chain_spec::ChainSpec::from_json_bytes(&chain_spec) {
        Ok(cs) => cs,
        Err(err) => {
            let msg = format!("Error while opening chain specs: {}", err);
            return Err(JsValue::from_str(&msg));
        }
    };

    /*let database = {
        let db_name = format!("substrate-lite-{}", chain_spec.id());
        database::indexed_db_light::Database::open(&db_name)
            .await
            .unwrap()
    };*/

    let (to_sync_tx, to_sync_rx) = mpsc::channel(64);
    let (to_network_tx, to_network_rx) = mpsc::channel(64);

    wasm_bindgen_futures::spawn_local(start_network(&chain_spec, to_network_rx, to_sync_tx).await);
    wasm_bindgen_futures::spawn_local(start_sync(&chain_spec, to_sync_rx, to_network_tx).await);

    Ok(BrowserLightClient {})
}

#[wasm_bindgen]
impl BrowserLightClient {
    /// Starts an RPC request. Returns a `Promise` containing the result of that request.
    #[wasm_bindgen(js_name = "rpcSend")]
    pub fn rpc_send(&mut self, rpc: &str) -> Result<js_sys::Promise, JsValue> {
        let call = json_rpc::methods::parse_json_call(rpc)
            .map_err(|err| JsValue::from_str(&err.to_string()))?;

        // TODO: testing
        return Err(JsValue::from_str(&format!(" test {:?}", call)));

        Ok(wasm_bindgen_futures::future_to_promise(async move {
            // TODO:
            loop {
                futures::pending!()
            }
        }))
    }

    /// Subscribes to an RPC pubsub endpoint.
    #[wasm_bindgen(js_name = "rpcSubscribe")]
    pub fn rpc_subscribe(&mut self, rpc: &str, callback: js_sys::Function) -> Result<(), JsValue> {
        // TODO:

        Ok(())
    }
}

async fn start_sync(
    chain_spec: &chain_spec::ChainSpec,
    mut to_sync: mpsc::Receiver<ToSync>,
    mut to_network: mpsc::Sender<ToNetwork>,
) -> impl Future<Output = ()> {
    let babe_genesis_config = babe::BabeGenesisConfiguration::from_genesis_storage(|k| {
        chain_spec
            .genesis_storage()
            .find(|(k2, _)| *k2 == k)
            .map(|(_, v)| v.to_owned())
    })
    .unwrap();

    let grandpa_genesis_config =
        grandpa::chain_config::GrandpaGenesisConfiguration::from_genesis_storage(|k| {
            chain_spec
                .genesis_storage()
                .find(|(k2, _)| *k2 == k)
                .map(|(_, v)| v.to_owned())
        })
        .unwrap();

    let mut sync = headers_optimistic::OptimisticHeadersSync::<network::PeerId>::new(
        headers_optimistic::Config {
            chain_config: chain::blocks_tree::Config {
                // TODO: load from database
                chain_information: chain::blocks_tree::ChainInformation {
                    finalized_block_header: crate::calculate_genesis_block_scale_encoded_header(
                        chain_spec.genesis_storage(),
                    ),
                    babe_finalized_block1_slot_number: None,
                    babe_finalized_block_epoch_information: None,
                    babe_finalized_next_epoch_transition: None,
                    grandpa_after_finalized_block_authorities_set_id: 0,
                    grandpa_finalized_scheduled_changes: Vec::new(),
                    grandpa_finalized_triggered_authorities: grandpa_genesis_config
                        .initial_authorities,
                },
                babe_genesis_config,
                blocks_capacity: {
                    // This capacity should be the maximum interval between two justifications.
                    1024
                },
            },
            sources_capacity: 32,
            blocks_request_granularity: NonZeroU32::new(128).unwrap(),
            download_ahead_blocks: {
                // Assuming a verification speed of 1k blocks/sec and a 95% latency of one second,
                // the number of blocks to download ahead of time in order to not block is 1000.
                1024
            },
        },
    );

    async move {
        // TODO: store request id in a sync user data rather than having this hashmap
        let mut block_requests_ids = hashbrown::HashMap::<_, _, fnv::FnvBuildHasher>::default();
        let mut block_requests_finished = stream::FuturesUnordered::new();

        loop {
            for action in sync.requests_actions() {
                match action {
                    headers_optimistic::RequestAction::Start {
                        request_id,
                        block_height,
                        source,
                        num_blocks,
                    } => {
                        let (tx, rx) = oneshot::channel();
                        let _ = to_network
                            .send(ToNetwork::StartBlockRequest {
                                peer_id: source.clone(),
                                block_height,
                                num_blocks: num_blocks.get(),
                                send_back: tx,
                            })
                            .await;

                        let (rx, abort) = future::abortable(rx);
                        block_requests_ids.insert(request_id, abort);
                        block_requests_finished.push(rx.map(move |r| (request_id, r)));
                    }
                    headers_optimistic::RequestAction::Cancel { request_id, source } => {
                        block_requests_ids.remove(&request_id).unwrap().abort();
                    }
                }
            }

            futures::select! {
                message = to_sync.next() => {
                    let message = match message {
                        Some(m) => m,
                        None => return,
                    };

                    match message {
                        ToSync::NewPeer(peer_id) => {
                            sync.add_source(peer_id).unwrap();
                        },
                        ToSync::PeerDisconnected(peer_id) => {
                            sync.remove_source(&peer_id).unwrap();
                        },
                    }
                },

                (request_id, result) = block_requests_finished.select_next_some() => {
                    // `result` is an error if the block request got cancelled by the sync state
                    // machine.
                    if let Ok(result) = result {
                        let _ = sync.finish_request(request_id, result.unwrap().map(|v| v.into_iter()));
                    }
                },

                // Dummy future that is constantly pending if and only if the sync'ing has
                // nothing to process.
                chain_state_update = async {
                    if let Some(outcome) = sync.process_one() {
                        outcome
                    } else {
                        loop { futures::pending!() }
                    }
                }.fuse() => {
                    web_sys::console::log_1(&JsValue::from_str(&format!(
                        "Chain state update: {:?}", chain_state_update
                    )));
                },
            }
        }
    }
}

enum ToSync {
    NewPeer(network::PeerId),
    PeerDisconnected(network::PeerId),
}

async fn start_network(
    chain_spec: &chain_spec::ChainSpec,
    mut to_network: mpsc::Receiver<ToNetwork>,
    mut to_sync: mpsc::Sender<ToSync>,
) -> impl Future<Output = ()> {
    let mut network = {
        let mut known_addresses = chain_spec
            .boot_nodes()
            .iter()
            .map(|bootnode_str| network::parse_str_addr(bootnode_str).unwrap())
            .collect::<Vec<_>>();
        // TODO: remove this; temporary because bootnode is apparently full
        // TODO: to test, run a node with ./target/debug/polkadot --chain kusama --listen-addr /ip4/0.0.0.0/tcp/30333/ws --node-key 0000000000000000000000000000000000000000000000000000000000000000
        known_addresses.push((
            "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN"
                .parse()
                .unwrap(),
            "/ip4/127.0.0.1/tcp/30333/ws".parse().unwrap(),
        ));

        network::Network::start(network::Config {
            known_addresses,
            chain_spec_protocol_id: chain_spec.protocol_id().as_bytes().to_vec(),
            tasks_executor: Box::new(|fut| wasm_bindgen_futures::spawn_local(fut)),
            local_genesis_hash: crate::calculate_genesis_block_hash(chain_spec.genesis_storage())
                .into(),
            wasm_external_transport: Some(ExtTransport::new(ffi::websocket_transport())),
        })
        .await
    };

    async move {
        // TODO: store send back channel in a network user data rather than having this hashmap
        let mut block_requests = hashbrown::HashMap::<_, _, fnv::FnvBuildHasher>::default();

        loop {
            futures::select! {
                message = to_network.next() => {
                    let message = match message {
                        Some(m) => m,
                        None => return,
                    };

                    match message {
                        ToNetwork::StartBlockRequest { peer_id, block_height, num_blocks, send_back } => {
                            let id = network.start_block_request(network::BlocksRequestConfig {
                                start: network::BlocksRequestConfigStart::Number(block_height),
                                peer_id,
                                desired_count: num_blocks,
                                direction: network::BlocksRequestDirection::Ascending,
                                fields: network::BlocksRequestFields {
                                    header: true,
                                    body: false,
                                    justification: true,
                                },
                            }).await.unwrap(); // TODO: don't unwrap

                            block_requests.insert(id, send_back);
                        },
                    }
                },

                event = network.next_event().fuse() => {
                    match event {
                        network::Event::BlockAnnounce(header) => {
                            // TODO:
                            if let Ok(decoded) = header::decode(&header.0) {
                                web_sys::console::log_1(&JsValue::from_str(&format!(
                                    "Received block announce: {}", decoded.number
                                )));
                            }
                        }
                        network::Event::BlocksRequestFinished { id, result } => {
                            let send_back = block_requests.remove(&id).unwrap();
                            let _: Result<_, _> = send_back.send(result
                                .map(|list| {
                                    list.into_iter().map(|block| {
                                        headers_optimistic::RequestSuccessBlock {
                                            scale_encoded_header: block.header.unwrap().0,
                                            scale_encoded_justification: block.justification,
                                        }
                                    }).collect()
                                })
                                .map_err(|()| headers_optimistic::RequestFail::BlocksUnavailable) // TODO:
                            );
                        }
                        network::Event::Connected(peer_id) => {
                            let _ = to_sync.send(ToSync::NewPeer(peer_id)).await;
                        }
                        network::Event::Disconnected(peer_id) => {
                            let _ = to_sync.send(ToSync::PeerDisconnected(peer_id)).await;
                        }
                    }
                },
            }
        }
    }
}

enum ToNetwork {
    StartBlockRequest {
        peer_id: network::PeerId,
        block_height: NonZeroU64,
        num_blocks: u32,
        send_back: oneshot::Sender<
            Result<Vec<headers_optimistic::RequestSuccessBlock>, headers_optimistic::RequestFail>,
        >,
    },
}
