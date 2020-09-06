#![recursion_limit = "1024"]

use futures::{
    channel::{mpsc, oneshot},
    lock::Mutex,
    prelude::*,
};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    thread,
    time::Duration,
};
use substrate_lite::{
    chain::{self, sync::full_optimistic},
    chain_spec, header, network,
    verify::babe,
};

fn main() {
    env_logger::init();
    futures::executor::block_on(async_main())
}

async fn async_main() {
    let chain_spec = substrate_lite::chain_spec::ChainSpec::from_json_bytes(
        &include_bytes!("../polkadot.json")[..],
    )
    .unwrap();

    const APP_INFO: app_dirs::AppInfo = app_dirs::AppInfo {
        name: "substrate-lite",
        author: "tomaka17",
    };

    let database = {
        let db_path =
            app_dirs::app_dir(app_dirs::AppDataType::UserData, &APP_INFO, "database").unwrap();
        let database = open_database(db_path.join(chain_spec.id())).await.unwrap();
        substrate_lite::database_open_match_chain_specs(database, &chain_spec).unwrap()
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
            chain::chain_information::ChainInformation::from_genesis_storage(
                chain_spec.genesis_storage(),
            )
            .unwrap()
        //}
    ; //};

    let (to_sync_tx, to_sync_rx) = mpsc::channel(64);
    let (to_network_tx, to_network_rx) = mpsc::channel(64);
    let (to_db_save_tx, mut to_db_save_rx) = mpsc::channel(16);

    threads_pool.spawn_ok({
        let tasks_executor = Box::new({
            let threads_pool = threads_pool.clone();
            move |f| threads_pool.spawn_ok(f)
        });

        start_network(&chain_spec, tasks_executor, to_network_rx, to_sync_tx).await
    });

    let sync_state = Arc::new(Mutex::new(SyncState {
        best_block_hash: [0; 32], // TODO:
        best_block_number: 0,     // TODO:
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
                // TODO:
                // We end the informant line with a `\r` so that it overwrites itself every time.
                // If any other line gets printed, it will overwrite the informant, and the
                // informant will then print itself below, which is a fine behaviour.
                let sync_state = sync_state.lock().await;
                eprint!("{}\r", substrate_lite::informant::InformantLine {
                    chain_name: chain_spec.name(),
                    max_line_width: terminal_size::terminal_size().map(|(w, _)| w.0.into()).unwrap_or(80),
                    num_network_connections: 0, // TODO: service.num_network_connections(),
                    best_number: sync_state.best_block_number,
                    finalized_number: sync_state.best_block_number, // TODO:
                    best_hash: &sync_state.best_block_hash,
                    finalized_hash: &sync_state.best_block_hash, // TODO:
                    network_known_best: None, // TODO:
                });
            }
            telemetry_event = telemetry.next_event().fuse() => {
                // TODO:
                /*telemetry.send(substrate_lite::telemetry::message::TelemetryMessage::SystemConnected(substrate_lite::telemetry::message::SystemConnected {
                    chain: chain_spec.name().to_owned().into_boxed_str(),
                    name: String::from("Polkadot ✨ lite ✨").into_boxed_str(),  // TODO: node name
                    implementation: String::from("Secret projet 🤫").into_boxed_str(),  // TODO:
                    version: String::from("1.0.0").into_boxed_str(),   // TODO: version
                    validator: None,
                    network_id: Some(service.local_peer_id().to_base58().into_boxed_str()),
                }));*/
            },
            _ = telemetry_timer.next() => {
                // TODO:
                /*// Some of the fields below are set to `None` because there is no plan to
                // implement reporting accurate metrics about the node.
                telemetry.send(substrate_lite::telemetry::message::TelemetryMessage::SystemInterval(substrate_lite::telemetry::message::SystemInterval {
                    stats: substrate_lite::telemetry::message::NodeStats {
                        peers: service.num_network_connections(),
                        txcount: 0,  // TODO:
                    },
                    memory: None,
                    cpu: None,
                    bandwidth_upload: Some(0.0), // TODO:
                    bandwidth_download: Some(0.0), // TODO:
                    finalized_height: Some(service.finalized_block_number()),
                    finalized_hash: Some(service.finalized_block_hash().into()),
                    block: substrate_lite::telemetry::message::Block {
                        hash: service.best_block_hash().into(),
                        height: service.best_block_number(),
                    },
                    used_state_cache_size: None,
                    used_db_cache_size: None,
                    disk_read_per_sec: None,
                    disk_write_per_sec: None,
                }));*/
            }
        }
    }
}

async fn start_sync(
    chain_spec: &chain_spec::ChainSpec,
    chain_information: chain::chain_information::ChainInformation,
    sync_state: Arc<Mutex<SyncState>>,
    mut to_sync: mpsc::Receiver<ToSync>,
    mut to_network: mpsc::Sender<ToNetwork>,
    mut to_db_save_tx: mpsc::Sender<chain::chain_information::ChainInformation>,
) -> impl Future<Output = ()> {
    let babe_genesis_config = babe::BabeGenesisConfiguration::from_genesis_storage(|k| {
        chain_spec
            .genesis_storage()
            .find(|(k2, _)| *k2 == k)
            .map(|(_, v)| v.to_owned())
    })
    .unwrap();

    let mut sync =
        full_optimistic::OptimisticFullSync::<_, network::PeerId>::new(full_optimistic::Config {
            chain_information,
            babe_genesis_config,
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
                    full_optimistic::ProcessOne::Finished { sync: s } => {
                        sync = s;
                        break;
                    }
                    full_optimistic::ProcessOne::FinalizedStorageGet(mut req) => {
                        // TODO: we shouldn't have to do this folding
                        let key = req.key().fold(Vec::new(), |mut a, b| {
                            a.extend_from_slice(b.as_ref());
                            a
                        });

                        let value = finalized_block_storage.get(&key).map(|v| &v[..]);
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
                *lock = SyncState {
                    best_block_hash: sync.best_block_hash(),
                    best_block_number: sync.best_block_number(),
                };
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
}

async fn start_network(
    chain_spec: &chain_spec::ChainSpec,
    tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
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
                        }
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
            Result<Vec<full_optimistic::RequestSuccessBlock>, full_optimistic::RequestFail>,
        >,
    },
}

/// Since opening the database can take a long time, this utility function performs this operation
/// in the background while showing a small progress bar to the user.
// TODO: shouldn't expose `sled`
async fn open_database(
    path: PathBuf,
) -> Result<substrate_lite::database::sled::DatabaseOpen, sled::Error> {
    let (tx, rx) = oneshot::channel();
    let mut rx = rx.fuse();

    let thread_spawn_result = thread::Builder::new().name("database-open".into()).spawn({
        let path = path.clone();
        move || {
            let result =
                substrate_lite::database::sled::open(substrate_lite::database::sled::Config {
                    path: &path,
                });
            let _ = tx.send(result);
        }
    });

    if thread_spawn_result.is_err() {
        return substrate_lite::database::sled::open(substrate_lite::database::sled::Config {
            path: &path,
        });
    }

    let mut progress_timer = stream::unfold((), move |_| {
        futures_timer::Delay::new(Duration::from_millis(200)).map(|_| Some(((), ())))
    })
    .map(|_| ());

    let mut next_progress_icon = ['-', '\\', '|', '/'].iter().cloned().cycle();

    loop {
        futures::select! {
            res = rx => return res.unwrap(),
            _ = progress_timer.next() => {
                eprint!("    Opening database... {}\r", next_progress_icon.next().unwrap());
            }
        }
    }
}
