// Copyright (C) 2019-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![recursion_limit = "1024"]

use atomic::Atomic;
use futures::{
    channel::{mpsc, oneshot},
    lock::Mutex,
    prelude::*,
};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    fs,
    net::{SocketAddr, ToSocketAddrs as _},
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
    pin::Pin,
    sync::{atomic::Ordering, Arc},
    thread,
    time::Duration,
};
use structopt::StructOpt as _;
use substrate_lite::{
    chain::{self, sync::full_optimistic},
    chain_spec, header, network,
};

fn main() {
    env_logger::init();
    futures::executor::block_on(async_main())
}

#[derive(Debug, structopt::StructOpt)]
struct CliOptions {
    /// Chain to connect to ("polkadot", "kusama", "westend", or a file path).
    #[structopt(long, default_value = "polkadot")]
    chain: CliChain,
}

#[derive(Debug)]
enum CliChain {
    Polkadot,
    Kusama,
    Westend,
    Custom(PathBuf),
}

impl core::str::FromStr for CliChain {
    type Err = core::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "polkadot" {
            Ok(CliChain::Polkadot)
        } else if s == "kusama" {
            Ok(CliChain::Kusama)
        } else if s == "westend" {
            Ok(CliChain::Westend)
        } else {
            Ok(CliChain::Custom(s.parse()?))
        }
    }
}

async fn async_main() {
    let cli_options = CliOptions::from_args();

    let chain_spec = {
        let json: Cow<[u8]> = match cli_options.chain {
            CliChain::Polkadot => (&include_bytes!("../polkadot.json")[..]).into(),
            CliChain::Kusama => (&include_bytes!("../kusama.json")[..]).into(),
            CliChain::Westend => (&include_bytes!("../westend.json")[..]).into(),
            CliChain::Custom(path) => fs::read(&path).expect("Failed to read chain specs").into(),
        };

        substrate_lite::chain_spec::ChainSpec::from_json_bytes(&json)
            .expect("Failed to decode chain specs")
    };

    let threads_pool = futures::executor::ThreadPool::builder()
        .name_prefix("tasks-pool-")
        .create()
        .unwrap();

    // Load the information about the chain from the database, or build the information of the
    // genesis block.
    // TODO:
    let chain_information = /*match local_storage.chain_information() {
        Ok(Some(i)) => i,
        Err(database::local_storage_light::AccessError::StorageAccess(err)) => return Err(err),
        // TODO: log why storage access failed?
        Err(database::local_storage_light::AccessError::Corrupted(_)) | Ok(None) => {*/
            chain::chain_information::ChainInformationConfig::from_genesis_storage(
                chain_spec.genesis_storage(),
            )
            .unwrap()
        //}
    ; //};

    // TODO: remove; just for testing
    /*let metadata = substrate_lite::metadata::metadata_from_runtime_code(
        chain_spec
            .genesis_storage()
            .clone()
            .find(|(k, _)| *k == b":code")
            .unwrap().1,
            1024,
    )
    .unwrap();
    println!(
        "{:#?}",
        substrate_lite::metadata::decode(&metadata).unwrap()
    );*/

    let (to_sync_tx, to_sync_rx) = mpsc::channel(64);
    let (to_network_tx, to_network_rx) = mpsc::channel(64);
    let (to_db_save_tx, mut to_db_save_rx) = mpsc::channel(16);

    let network_state = Arc::new(NetworkState {
        best_network_block_height: Atomic::new(0),
        num_network_connections: Atomic::new(0),
    });

    threads_pool.spawn_ok({
        let tasks_executor = Box::new({
            let threads_pool = threads_pool.clone();
            move |f| threads_pool.spawn_ok(f)
        });

        start_network(
            &chain_spec,
            tasks_executor,
            network_state.clone(),
            to_network_rx,
            to_sync_tx.clone(), // TODO: don't clone
        )
        .await
    });

    let sync_state = Arc::new(Mutex::new(SyncState {
        best_block_hash: [0; 32],      // TODO:
        best_block_number: 0,          // TODO:
        finalized_block_hash: [0; 32], // TODO:
        finalized_block_number: 0,     // TODO:
    }));

    threads_pool.spawn_ok(
        start_sync(
            &chain_spec,
            chain_information,
            sync_state.clone(),
            to_sync_rx,
            to_network_tx,
            to_db_save_tx,
        )
        .await,
    );

    threads_pool.spawn_ok(async move {
        while let Some(info) = to_db_save_rx.next().await {
            // TODO:
        }
    });

    let mut telemetry = {
        let endpoints = chain_spec
            .telemetry_endpoints()
            .map(|addr| (addr.as_ref().to_owned(), 0))
            .collect::<Vec<_>>();

        substrate_lite::telemetry::init_telemetry(substrate_lite::telemetry::TelemetryConfig {
            endpoints: substrate_lite::telemetry::TelemetryEndpoints::new(endpoints).unwrap(),
            wasm_external_transport: None,
            tasks_executor: {
                let threads_pool = threads_pool.clone();
                Box::new(move |task| threads_pool.spawn_obj_ok(From::from(task))) as Box<_>
            },
        })
    };

    let mut informant_timer = stream::unfold((), move |_| {
        futures_timer::Delay::new(Duration::from_secs(1)).map(|_| Some(((), ())))
    })
    .map(|_| ());

    let mut telemetry_timer = stream::unfold((), move |_| {
        futures_timer::Delay::new(Duration::from_secs(5)).map(|_| Some(((), ())))
    })
    .map(|_| ());

    loop {
        futures::select! {
            _ = informant_timer.next() => {
                // We end the informant line with a `\r` so that it overwrites itself every time.
                // If any other line gets printed, it will overwrite the informant, and the
                // informant will then print itself below, which is a fine behaviour.
                let sync_state = sync_state.lock().await.clone();
                eprint!("{}\r", substrate_lite::informant::InformantLine {
                    chain_name: chain_spec.name(),
                    max_line_width: terminal_size::terminal_size().map(|(w, _)| w.0.into()).unwrap_or(80),
                    num_network_connections: network_state.num_network_connections.load(Ordering::Relaxed),
                    best_number: sync_state.best_block_number,
                    finalized_number: sync_state.finalized_block_number,
                    best_hash: &sync_state.best_block_hash,
                    finalized_hash: &sync_state.finalized_block_hash,
                    network_known_best: match network_state.best_network_block_height.load(Ordering::Relaxed) {
                        0 => None,
                        n => Some(n)
                    },
                });
            },

            telemetry_event = telemetry.next_event().fuse() => {
                telemetry.send(substrate_lite::telemetry::message::TelemetryMessage::SystemConnected(substrate_lite::telemetry::message::SystemConnected {
                    chain: chain_spec.name().to_owned().into_boxed_str(),
                    name: String::from("Polkadot ✨ lite ✨").into_boxed_str(),  // TODO: node name
                    implementation: String::from("Secret projet 🤫").into_boxed_str(),  // TODO:
                    version: String::from(env!("CARGO_PKG_VERSION")).into_boxed_str(),
                    validator: None,
                    network_id: None, // TODO: Some(service.local_peer_id().to_base58().into_boxed_str()),
                }));
            },

            _ = telemetry_timer.next() => {
                let sync_state = sync_state.lock().await.clone();

                // Some of the fields below are set to `None` because there is no plan to
                // implement reporting accurate metrics about the node.
                telemetry.send(substrate_lite::telemetry::message::TelemetryMessage::SystemInterval(substrate_lite::telemetry::message::SystemInterval {
                    stats: substrate_lite::telemetry::message::NodeStats {
                        peers: network_state.num_network_connections.load(Ordering::Relaxed),
                        txcount: 0,  // TODO:
                    },
                    memory: None,
                    cpu: None,
                    bandwidth_upload: Some(0.0), // TODO:
                    bandwidth_download: Some(0.0), // TODO:
                    finalized_height: Some(sync_state.finalized_block_number),
                    finalized_hash: Some(sync_state.finalized_block_hash.into()),
                    block: substrate_lite::telemetry::message::Block {
                        hash: sync_state.best_block_hash.into(),
                        height: sync_state.best_block_number,
                    },
                    used_state_cache_size: None,
                    used_db_cache_size: None,
                    disk_read_per_sec: None,
                    disk_write_per_sec: None,
                }));
            },
        }
    }
}

async fn start_sync(
    chain_spec: &chain_spec::ChainSpec,
    chain_information_config: chain::chain_information::ChainInformationConfig,
    sync_state: Arc<Mutex<SyncState>>,
    mut to_sync: mpsc::Receiver<ToSync>,
    mut to_network: mpsc::Sender<ToNetwork>,
    mut to_db_save_tx: mpsc::Sender<chain::chain_information::ChainInformation>,
) -> impl Future<Output = ()> {
    let mut sync =
        full_optimistic::OptimisticFullSync::<_, network::PeerId>::new(full_optimistic::Config {
            chain_information_config,
            sources_capacity: 32,
            blocks_capacity: {
                // This is the maximum number of blocks between two consecutive justifications.
                1024
            },
            source_selection_randomness_seed: rand::random(),
            blocks_request_granularity: NonZeroU32::new(128).unwrap(),
            download_ahead_blocks: {
                // Assuming a verification speed of 1k blocks/sec and a 95% latency of one second,
                // the number of blocks to download ahead of time in order to not block is 1000.
                1024
            },
        });

    let mut finalized_block_storage = BTreeMap::<Vec<u8>, Vec<u8>>::new();
    // TODO: doesn't necessarily match chain_information; pass this as part of the params of `start_sync` instead
    for (key, value) in chain_spec.genesis_storage() {
        finalized_block_storage.insert(key.to_owned(), value.to_owned());
    }

    async move {
        let mut peers_source_id_map = hashbrown::HashMap::<_, _, fnv::FnvBuildHasher>::default();
        let mut block_requests_finished = stream::FuturesUnordered::new();

        loop {
            // Verify blocks that have been fetched from queries.
            let mut process = sync.process_one();
            loop {
                match process {
                    full_optimistic::ProcessOne::Finished {
                        sync: s,
                        finalized_blocks,
                    } => {
                        if let Some(last_finalized) = finalized_blocks.last() {
                            let mut lock = sync_state.lock().await;
                            lock.finalized_block_hash = last_finalized.header.hash();
                            lock.finalized_block_number = last_finalized.header.number;
                        }

                        for block in finalized_blocks {
                            for (key, value) in block.storage_top_trie_changes {
                                if let Some(value) = value {
                                    finalized_block_storage.insert(key, value);
                                } else {
                                    let _was_there = finalized_block_storage.remove(&key);
                                    // TODO: panics?! assert!(_was_there.is_some());
                                }
                            }
                        }

                        sync = s;
                        break;
                    }

                    full_optimistic::ProcessOne::InProgress {
                        current_best_hash,
                        current_best_number,
                        resume,
                    } => {
                        // Processing has made a step forward.
                        // There is nothing to do, but this is used to update to best block
                        // shown on the informant.
                        let mut lock = sync_state.lock().await;
                        lock.best_block_hash = current_best_hash;
                        lock.best_block_number = current_best_number;
                        drop(lock);

                        process = resume.resume();
                    }

                    full_optimistic::ProcessOne::FinalizedStorageGet(mut req) => {
                        let value = finalized_block_storage
                            .get(&req.key_as_vec())
                            .map(|v| &v[..]);
                        process = req.inject_value(value);
                    }
                    full_optimistic::ProcessOne::FinalizedStorageNextKey(mut req) => {
                        // TODO: to_vec() :-/
                        let next_key = finalized_block_storage
                            .range(req.key().to_vec()..)
                            .skip(1)
                            .next()
                            .map(|(k, _)| k);
                        process = req.inject_key(next_key);
                    }
                    full_optimistic::ProcessOne::FinalizedStoragePrefixKeys(mut req) => {
                        // TODO: to_vec() :-/
                        let prefix = req.prefix().to_vec();
                        // TODO: to_vec() :-/
                        let keys = finalized_block_storage
                            .range(req.prefix().to_vec()..)
                            .take_while(|(k, _)| k.starts_with(&prefix))
                            .map(|(k, _)| k);
                        process = req.inject_keys(keys);
                    }
                }
            }

            // Update the current best block, used for CLI-related purposes.
            {
                let mut lock = sync_state.lock().await;
                lock.best_block_hash = sync.best_block_hash();
                lock.best_block_number = sync.best_block_number();
            }

            // Start requests that need to be started.
            // Note that this is done after calling `process_one`, as the processing of pending
            // blocks can result in new requests but not the contrary.
            while let Some(action) = sync.next_request_action() {
                match action {
                    full_optimistic::RequestAction::Start {
                        start,
                        block_height,
                        source,
                        num_blocks,
                        ..
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
                        let request_id = start.start(abort);
                        block_requests_finished.push(rx.map(move |r| (request_id, r)));
                    }
                    full_optimistic::RequestAction::Cancel { user_data, .. } => {
                        user_data.abort();
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
                        let _ = sync.finish_request(request_id, result.unwrap().map(|v| v.into_iter()));
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

#[derive(Debug, Clone)]
struct SyncState {
    best_block_number: u64,
    best_block_hash: [u8; 32],
    finalized_block_number: u64,
    finalized_block_hash: [u8; 32],
}

async fn start_network(
    chain_spec: &chain_spec::ChainSpec,
    tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
    network_state: Arc<NetworkState>,
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
            tasks_executor,
            local_genesis_hash: substrate_lite::calculate_genesis_block_header(
                chain_spec.genesis_storage(),
            )
            .hash(),
            wasm_external_transport: None,
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
                            let result = network.start_block_request(network::BlocksRequestConfig {
                                start: network::BlocksRequestConfigStart::Number(block_height),
                                peer_id,
                                desired_count: num_blocks,
                                direction: network::BlocksRequestDirection::Ascending,
                                fields: network::BlocksRequestFields {
                                    header: true,
                                    body: true,
                                    justification: true,
                                },
                            }).await;

                            match result {
                                Ok(id) => {
                                    block_requests.insert(id, send_back);
                                }
                                Err(()) => {
                                    // TODO: better error
                                    let _ = send_back.send(Err(full_optimistic::RequestFail::BlocksUnavailable));
                                }
                            };
                        },
                    }
                },

                event = network.next_event().fuse() => {
                    match event {
                        network::Event::BlockAnnounce(header) => {
                            if let Ok(header) = header::decode(&header.0) {
                                network_state.best_network_block_height.store(header.number, Ordering::Relaxed);
                            }
                        }
                        network::Event::CallRequestFinished { .. } => unreachable!(),
                        network::Event::BlocksRequestFinished { id, result } => {
                            let send_back = block_requests.remove(&id).unwrap();
                            let _: Result<_, _> = send_back.send(result
                                .map(|list| {
                                    list.into_iter().map(|block| {
                                        full_optimistic::RequestSuccessBlock {
                                            scale_encoded_header: block.header.unwrap().0,
                                            scale_encoded_extrinsics: block.body.unwrap().into_iter().map(|e| e.0).collect(), // TODO: overhead
                                            scale_encoded_justification: block.justification,
                                        }
                                    }).collect()
                                })
                                .map_err(|()| full_optimistic::RequestFail::BlocksUnavailable) // TODO:
                            );
                        }
                        network::Event::Connected(peer_id) => {
                            network_state.num_network_connections.fetch_add(1, Ordering::Relaxed);
                            let _ = to_sync.send(ToSync::NewPeer(peer_id)).await;
                        }
                        network::Event::Disconnected(peer_id) => {
                            network_state.num_network_connections.fetch_sub(1, Ordering::Relaxed);
                            let _ = to_sync.send(ToSync::PeerDisconnected(peer_id)).await;
                        }
                    }
                },
            }
        }
    }
}

#[derive(Debug)]
struct NetworkState {
    /// 0 means "unknown".
    best_network_block_height: Atomic<u64>,
    num_network_connections: Atomic<u64>,
}

enum ToNetwork {
    StartBlockRequest {
        peer_id: network::PeerId,
        block_height: NonZeroU64,
        num_blocks: u32,
        send_back: oneshot::Sender<
            Result<Vec<full_optimistic::RequestSuccessBlock>, full_optimistic::RequestFail>,
        >,
    },
}
