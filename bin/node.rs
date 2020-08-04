#![recursion_limit = "1024"]

use futures::{channel::oneshot, prelude::*};
use std::{cmp, path::PathBuf, thread, time::Duration};

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
        .name_prefix("thread-")
        .create()
        .unwrap();

    let mut service = substrate_lite::service::Config {
        database: Some(database),
        tasks_executor: {
            let threads_pool = threads_pool.clone();
            Box::new(move |task| threads_pool.spawn_obj_ok(From::from(task))) as Box<_>
        },
        chain_info: From::from(&chain_spec),
        wasm_external_transport: None,
    }
    .build()
    .await;

    let mut rpc_server = {
        let rpc_config = substrate_lite::rpc_server::Config {
            chain_name: chain_spec.name().to_owned(),
            chain_type: "Live".into(), // TODO: temporary
            chain_properties: chain_spec
                .properties()
                .filter_map(|(name, prop)| {
                    let prop = match prop {
                        serde_json::Value::String(s) => {
                            substrate_lite::rpc_server::ChainProperty::String(s.clone())
                        }
                        serde_json::Value::Number(n) => {
                            if let Some(n) = n.as_u64() {
                                substrate_lite::rpc_server::ChainProperty::Number(n)
                            } else {
                                return None;
                            }
                        }
                        _ => return None,
                    };

                    Some((name.to_owned(), prop))
                })
                .collect(),
            client_name: "Polkadot ✨ lite ✨".to_owned(),
            client_version: "??".to_owned(),
        };

        let mut server = substrate_lite::rpc_server::RpcServers::new(rpc_config);
        server
            .spawn_ws("0.0.0.0:9944".parse().unwrap())
            .await
            .unwrap();
        server
    };

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

    let mut network_known_best = None;

    loop {
        futures::select! {
            _ = informant_timer.next() => {
                // We end the informant line with a `\r` so that it overwrites itself every time.
                // If any other line gets printed, it will overwrite the informant, and the
                // informant will then print itself below, which is a fine behaviour.
                eprint!("{}\r", substrate_lite::informant::InformantLine {
                    chain_name: chain_spec.name(),
                    max_line_width: terminal_size::terminal_size().map(|(w, _)| w.0.into()).unwrap_or(80),
                    num_network_connections: service.num_network_connections(),
                    best_number: service.best_block_number(),
                    finalized_number: service.finalized_block_number(),
                    best_hash: &service.best_block_hash(),
                    finalized_hash: &service.finalized_block_hash(),
                    network_known_best,
                });
            }
            rpc_rq = rpc_server.next_event().fuse() => {
                match rpc_rq {
                    substrate_lite::rpc_server::Event::Request(rq) => {
                        rq.answer(&service).await;
                    }
                }
            }
            service_event = service.next_event().fuse() => {
                match service_event {
                    substrate_lite::service::Event::NewNetworkExternalAddress { address } => {
                        eprintln!("🔍 Discovered new external address for our node: {}", address);
                    }
                    substrate_lite::service::Event::BlockAnnounceReceived { number, .. } => {
                        if let Some(curr_best) = network_known_best {
                            network_known_best = Some(cmp::max(curr_best, number));
                        } else {
                            network_known_best = Some(number);
                        }
                    },
                    substrate_lite::service::Event::NewChainHead { number, hash, head_update, modified_keys } => {
                        rpc_server.notify_new_chain_head(hash.into(), modified_keys.iter().map(|k| &k[..]));

                        // When major syncing, the rate at which this event is generated is pretty
                        // high and sending a message to the telemetry server every single time
                        // would use a lot of bandwidth.
                        // TODO: make this cleaner
                        if rand::Rng::gen_bool(&mut rand::thread_rng(), 0.01) {
                            telemetry.send(substrate_lite::telemetry::message::TelemetryMessage::BlockImport(substrate_lite::telemetry::message::Block {
                                hash: hash.into(),
                                height: number,
                            }));
                        }
                    },
                    _ => {}
                }
            }
            telemetry_event = telemetry.next_event().fuse() => {
                telemetry.send(substrate_lite::telemetry::message::TelemetryMessage::SystemConnected(substrate_lite::telemetry::message::SystemConnected {
                    chain: chain_spec.name().to_owned().into_boxed_str(),
                    name: String::from("Polkadot ✨ lite ✨").into_boxed_str(),  // TODO: node name
                    implementation: String::from("Secret projet 🤫").into_boxed_str(),  // TODO:
                    version: String::from("1.0.0").into_boxed_str(),   // TODO: version
                    validator: None,
                    network_id: Some(service.local_peer_id().to_base58().into_boxed_str()),
                }));
            },
            _ = telemetry_timer.next() => {
                // Some of the fields below are set to `None` because there is no plan to
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
                }));
            }
        }
    }
}

/// Since opening the database can take a long time, this utility function performs this operation
/// in the background while showing a small progress bar to the user.
// TODO: shouldn't expose `sled`
async fn open_database(
    path: PathBuf,
) -> Result<substrate_lite::database::DatabaseOpen, sled::Error> {
    let (tx, rx) = oneshot::channel();
    let mut rx = rx.fuse();

    let thread_spawn_result = thread::Builder::new().name("database-open".into()).spawn({
        let path = path.clone();
        move || {
            let result =
                substrate_lite::database::open(substrate_lite::database::Config { path: &path });
            let _ = tx.send(result);
        }
    });

    if thread_spawn_result.is_err() {
        return substrate_lite::database::open(substrate_lite::database::Config { path: &path });
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
