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

#![recursion_limit = "1024"]
#![deny(broken_intra_doc_links)]
#![deny(unused_crate_dependencies)]

use futures::{channel::oneshot, prelude::*};
use smoldot::{
    chain, chain_spec,
    database::full_sqlite,
    header,
    informant::HashDisplay,
    libp2p::{connection, multiaddr, peer_id::PeerId},
};
use std::{
    borrow::Cow, convert::TryFrom as _, fs, io, iter, path::PathBuf, sync::Arc, thread,
    time::Duration,
};
use structopt::StructOpt as _;
use tracing::Instrument as _;

mod cli;
mod network_service;
mod sync_service;

fn main() {
    futures::executor::block_on(async_main())
}

async fn async_main() {
    let cli_options = cli::CliOptions::from_args();

    // Setup the logging system of the binary.
    if matches!(
        cli_options.output,
        cli::Output::Informant | cli::Output::Logs | cli::Output::LogsJson
    ) {
        let builder = tracing_subscriber::fmt()
            .with_timer(tracing_subscriber::fmt::time::ChronoUtc::rfc3339())
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::ACTIVE)
            .with_max_level(if matches!(cli_options.output, cli::Output::Informant) {
                tracing::Level::WARN // TODO: display warnings in a nicer way ; in particular, immediately put the informant on top of warnings
            } else {
                tracing::Level::TRACE // TODO: configurable?
            })
            .with_writer(io::stdout);

        // Because calling `builder.json()` changes the type of `builder`, we do it at the end
        // and call `init()` at the same time.
        //
        // This registers a global process-wide subscriber.
        // While this is poor programming practices and we would prefer using a crate that doesn't
        // rely on global variables, the `tracing` crate is currently one of the best logging
        // crates in the Rust ecosystem at the time of writing of this comment.
        if matches!(cli_options.output, cli::Output::LogsJson) {
            builder.json().init();
        } else {
            builder
                .with_ansi(match cli_options.color {
                    cli::ColorChoice::Always => true,
                    cli::ColorChoice::Never => false,
                })
                .init();
        }
    }

    let chain_spec = {
        let json: Cow<[u8]> = match &cli_options.chain {
            cli::CliChain::Polkadot => (&include_bytes!("../../polkadot.json")[..]).into(),
            cli::CliChain::Kusama => (&include_bytes!("../../kusama.json")[..]).into(),
            cli::CliChain::Westend => (&include_bytes!("../../westend.json")[..]).into(),
            cli::CliChain::Custom(path) => {
                fs::read(path).expect("Failed to read chain specs").into()
            }
        };

        smoldot::chain_spec::ChainSpec::from_json_bytes(&json)
            .expect("Failed to decode chain specs")
    };

    // TODO: don't unwrap?
    let genesis_chain_information =
        chain::chain_information::ChainInformation::from_chain_spec(&chain_spec).unwrap();

    // If `chain_spec` define a parachain, also load the specs of the relay chain.
    let (relay_chain_spec, _parachain_id) =
        if let Some((relay_chain_name, parachain_id)) = chain_spec.relay_chain() {
            let json: Cow<[u8]> = match &cli_options.chain {
                cli::CliChain::Custom(parachain_path) => {
                    // TODO: this is a bit of a hack
                    let relay_chain_path = parachain_path
                        .parent()
                        .unwrap()
                        .join(format!("{}.json", relay_chain_name));
                    fs::read(&relay_chain_path)
                        .expect("Failed to read relay chain specs")
                        .into()
                }
                _ => panic!("Unexpected relay chain specified in hard-coded specs"),
            };

            let spec = smoldot::chain_spec::ChainSpec::from_json_bytes(&json)
                .expect("Failed to decode relay chain chain specs");

            // Make sure we're not accidentally opening the same chain twice, otherwise weird
            // interactions will happen.
            assert_ne!(spec.id(), chain_spec.id());

            (Some(spec), Some(parachain_id))
        } else {
            (None, None)
        };

    let relay_genesis_chain_information = if let Some(relay_chain_spec) = &relay_chain_spec {
        // TODO: don't unwrap?
        Some(chain::chain_information::ChainInformation::from_chain_spec(relay_chain_spec).unwrap())
    } else {
        None
    };

    let threads_pool = futures::executor::ThreadPool::builder()
        .name_prefix("tasks-pool-")
        .create()
        .unwrap();

    let database = open_database(&chain_spec, &genesis_chain_information, cli_options.tmp).await;
    let relay_chain_database = if let Some(relay_chain_spec) = &relay_chain_spec {
        Some(
            open_database(
                &relay_chain_spec,
                relay_genesis_chain_information.as_ref().unwrap(),
                cli_options.tmp,
            )
            .await,
        )
    } else {
        None
    };

    // TODO: remove; just for testing
    /*let metadata = smoldot::metadata::metadata_from_runtime_code(
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
        smoldot::metadata::decode(&metadata).unwrap()
    );*/

    let (network_service, network_events_receivers) =
        network_service::NetworkService::new(network_service::Config {
            listen_addresses: Vec::new(),
            num_events_receivers: 2 + if relay_chain_database.is_some() { 1 } else { 0 },
            chains: iter::once(network_service::ChainConfig {
                protocol_id: chain_spec.protocol_id().to_owned(),
                has_grandpa_protocol: matches!(
                    genesis_chain_information.finality,
                    chain::chain_information::ChainInformationFinality::Grandpa { .. }
                ),
                genesis_block_hash: genesis_chain_information.finalized_block_header.hash(),
                best_block: {
                    let hash = database.finalized_block_hash().unwrap();
                    let header = database.block_scale_encoded_header(&hash).unwrap().unwrap();
                    let number = header::decode(&header).unwrap().number;
                    (number, hash)
                },
                bootstrap_nodes: {
                    let mut list = Vec::with_capacity(chain_spec.boot_nodes().len());
                    for node in chain_spec.boot_nodes().iter() {
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
            })
            .chain(
                relay_chain_spec
                    .as_ref()
                    .map(|relay_chains_specs| {
                        network_service::ChainConfig {
                            protocol_id: relay_chains_specs.protocol_id().to_owned(),
                            has_grandpa_protocol: matches!(
                                relay_genesis_chain_information.as_ref().unwrap().finality,
                                chain::chain_information::ChainInformationFinality::Grandpa { .. }
                            ),
                            genesis_block_hash: relay_genesis_chain_information
                                .as_ref()
                                .unwrap()
                                .finalized_block_header
                                .hash(),
                            best_block: {
                                let db = relay_chain_database.as_ref().unwrap();
                                let hash = db.finalized_block_hash().unwrap();
                                let header = db.block_scale_encoded_header(&hash).unwrap().unwrap();
                                let number = header::decode(&header).unwrap().number;
                                (number, hash)
                            },
                            bootstrap_nodes: {
                                let mut list =
                                    Vec::with_capacity(relay_chains_specs.boot_nodes().len());
                                for node in relay_chains_specs.boot_nodes().iter() {
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
                        }
                    })
                    .into_iter(),
            )
            .collect(),
            noise_key: if let Some(node_key) = cli_options.node_key {
                connection::NoiseKey::new(node_key.as_ref())
            } else {
                // TODO: load from disk or something instead
                connection::NoiseKey::new(&rand::random())
            },
            tasks_executor: {
                let threads_pool = threads_pool.clone();
                Box::new(move |task| threads_pool.spawn_ok(task))
            },
        })
        .instrument(tracing::debug_span!("network-service-init"))
        .await
        .unwrap();

    let mut network_events_receivers = network_events_receivers.into_iter();

    let sync_service = sync_service::SyncService::new(sync_service::Config {
        tasks_executor: {
            let threads_pool = threads_pool.clone();
            Box::new(move |task| threads_pool.spawn_ok(task))
        },
        network_events_receiver: network_events_receivers.next().unwrap(),
        network_service: (network_service.clone(), 0),
        database,
    })
    .instrument(tracing::debug_span!("sync-service-init"))
    .await;

    let relay_chain_sync_service = if let Some(relay_chain_database) = relay_chain_database {
        Some(
            sync_service::SyncService::new(sync_service::Config {
                tasks_executor: {
                    let threads_pool = threads_pool.clone();
                    Box::new(move |task| threads_pool.spawn_ok(task))
                },
                network_events_receiver: network_events_receivers.next().unwrap(),
                network_service: (network_service.clone(), 1),
                database: relay_chain_database,
            })
            .instrument(tracing::debug_span!("relay-chain-sync-service-init"))
            .await,
        )
    } else {
        None
    };

    /*let mut telemetry = {
        let endpoints = chain_spec
            .telemetry_endpoints()
            .map(|addr| (addr.as_ref().to_owned(), 0))
            .collect::<Vec<_>>();

        smoldot::telemetry::init_telemetry(smoldot::telemetry::TelemetryConfig {
            endpoints: smoldot::telemetry::TelemetryEndpoints::new(endpoints).unwrap(),
            wasm_external_transport: None,
            tasks_executor: {
                let threads_pool = threads_pool.clone();
                Box::new(move |task| threads_pool.spawn_obj_ok(From::from(task))) as Box<_>
            },
        })
    };*/

    // Starting from here, a SIGINT (or equivalent) handler is setup. If the user does Ctrl+C,
    // a message will be sent on `ctrlc_rx`.
    // This should be performed after all the expensive initialization is done, as otherwise the
    // fact that initialization isn't interrupted by Ctrl+C could be frustrating for the user, but
    // also as soon as possible, as we want as many parts as possible to be cleanly destroyed on
    // Ctrl+C.
    let mut ctrlc_rx = {
        let (tx, rx) = oneshot::channel();
        let mut tx = Some(tx);
        ctrlc::set_handler(move || {
            if let Some(tx) = tx.take() {
                let _ = tx.send(());
            }
        })
        .expect("Error setting Ctrl-C handler");
        rx.fuse()
    };

    let mut informant_timer = stream::once(future::ready(())).chain(
        stream::unfold((), move |_| {
            futures_timer::Delay::new(Duration::from_secs(1)).map(|_| Some(((), ())))
        })
        .map(|_| ()),
    );

    let mut telemetry_timer = stream::once(future::ready(())).chain(
        stream::unfold((), move |_| {
            futures_timer::Delay::new(Duration::from_secs(5)).map(|_| Some(((), ())))
        })
        .map(|_| ()),
    );

    let mut network_known_best = None;
    let mut main_network_events_receiver = network_events_receivers.next().unwrap();
    debug_assert!(network_events_receivers.next().is_none());

    loop {
        futures::select! {
            _ = informant_timer.next() => {
                if matches!(cli_options.output, cli::Output::Informant) {
                    // We end the informant line with a `\r` so that it overwrites itself every time.
                    // If any other line gets printed, it will overwrite the informant, and the
                    // informant will then print itself below, which is a fine behaviour.
                    let sync_state = sync_service.sync_state().await;
                    eprint!("{}\r", smoldot::informant::InformantLine {
                        enable_colors: match cli_options.color {
                            cli::ColorChoice::Always => true,
                            cli::ColorChoice::Never => false,
                        },
                        chain_name: chain_spec.name(),
                        relay_chain: if let Some(relay_chain_spec) = &relay_chain_spec {
                            let relay_sync_state = relay_chain_sync_service.as_ref().unwrap().sync_state().await;
                            Some(smoldot::informant::RelayChain {
                                chain_name: relay_chain_spec.name(),
                                best_number: relay_sync_state.best_block_number,
                            })
                        } else {
                            None
                        },
                        max_line_width: terminal_size::terminal_size().map(|(w, _)| w.0.into()).unwrap_or(80),
                        num_network_connections: u64::try_from(network_service.num_established_connections().await)
                            .unwrap_or(u64::max_value()),
                        best_number: sync_state.best_block_number,
                        finalized_number: sync_state.finalized_block_number,
                        best_hash: &sync_state.best_block_hash,
                        finalized_hash: &sync_state.finalized_block_hash,
                        network_known_best,
                    });
                }
            },

            network_message = main_network_events_receiver.next() => {
                if let network_service::Event::BlockAnnounce { chain_index: 0, announce, .. } = network_message.unwrap() {
                    let decoded = announce.decode();
                    match network_known_best {
                        Some(n) if n >= decoded.header.number => {},
                        _ => network_known_best = Some(decoded.header.number),
                    }
                }
            }

            /*telemetry_event = telemetry.next_event().fuse() => {
                telemetry.send(smoldot::telemetry::message::TelemetryMessage::SystemConnected(smoldot::telemetry::message::SystemConnected {
                    chain: chain_spec.name().to_owned().into_boxed_str(),
                    name: String::from("✨ Smoldot ✨").into_boxed_str(),  // TODO: node name
                    implementation: String::from("Secret projet 🤫").into_boxed_str(),  // TODO:
                    version: String::from(env!("CARGO_PKG_VERSION")).into_boxed_str(),
                    validator: None,
                    network_id: None, // TODO: Some(service.local_peer_id().to_base58().into_boxed_str()),
                }));
            },*/

            _ = telemetry_timer.next() => {
                /*let sync_state = sync_state.lock().await.clone();

                // Some of the fields below are set to `None` because there is no plan to
                // implement reporting accurate metrics about the node.
                telemetry.send(smoldot::telemetry::message::TelemetryMessage::SystemInterval(smoldot::telemetry::message::SystemInterval {
                    stats: smoldot::telemetry::message::NodeStats {
                        peers: network_state.num_network_connections.load(Ordering::Relaxed),
                        txcount: 0,  // TODO:
                    },
                    memory: None,
                    cpu: None,
                    bandwidth_upload: Some(0.0), // TODO:
                    bandwidth_download: Some(0.0), // TODO:
                    finalized_height: Some(sync_state.finalized_block_number),
                    finalized_hash: Some(sync_state.finalized_block_hash.into()),
                    block: smoldot::telemetry::message::Block {
                        hash: sync_state.best_block_hash.into(),
                        height: sync_state.best_block_number,
                    },
                    used_state_cache_size: None,
                    used_db_cache_size: None,
                    disk_read_per_sec: None,
                    disk_write_per_sec: None,
                }));*/
            },

            _ = ctrlc_rx => {
                // Adding a new line after the informant so that the user's shell doesn't
                // overwrite it.
                eprintln!("");
                return
            },
        }
    }
}

/// Opens the database from the filesystem, or create a new database if none is found.
///
/// If `tmp` is `true`, open the database in memory instead.
///
/// # Panic
///
/// Panics if the database can't be open. This function is expected to be called from the `main`
/// function.
///
#[tracing::instrument(skip(chain_spec))]
async fn open_database(
    chain_spec: &chain_spec::ChainSpec,
    genesis_chain_information: &chain::chain_information::ChainInformation,
    tmp: bool,
) -> Arc<full_sqlite::SqliteFullDatabase> {
    Arc::new({
        // Directory supposed to contain the database.
        let db_path = if !tmp {
            if let Some(base) = directories::ProjectDirs::from("io", "paritytech", "smoldot") {
                Some(base.data_dir().join(chain_spec.id()).join("database"))
            } else {
                tracing::warn!(
                    "Failed to fetch $HOME directory. Falling back to a temporary database. \
                    If this is intended, please make this explicit by passing the `--tmp` flag \
                    instead."
                );
                None
            }
        } else {
            None
        };

        // The `unwrap()` here can panic for example in case of access denied.
        match background_open_database(db_path.clone()).await.unwrap() {
            // Database already exists and contains data.
            full_sqlite::DatabaseOpen::Open(database) => {
                if database.block_hash_by_number(0).unwrap().next().unwrap()
                    != genesis_chain_information.finalized_block_header.hash()
                {
                    panic!(
                        "Mismatch between database and chain specification. Shutting down node."
                    );
                }

                let finalized_block_hash = database.finalized_block_hash().unwrap();
                let finalized_block = database
                    .block_scale_encoded_header(&finalized_block_hash)
                    .unwrap()
                    .unwrap();
                eprintln!(
                    "Loaded existing database (finalized: #{}, {})",
                    header::decode(&finalized_block).unwrap().number,
                    HashDisplay(&finalized_block_hash)
                );
                database
            }

            // The database doesn't exist or is empty.
            full_sqlite::DatabaseOpen::Empty(empty) => {
                // The finalized block is the genesis block. As such, it has an empty body and
                // no justification.
                empty
                    .initialize(
                        genesis_chain_information,
                        iter::empty(),
                        None,
                        chain_spec.genesis_storage(),
                    )
                    .unwrap()
            }
        }
    })
}

/// Since opening the database can take a long time, this utility function performs this operation
/// in the background while showing a small progress bar to the user.
///
/// If `path` is `None`, the database is opened in memory.
#[tracing::instrument]
async fn background_open_database(
    path: Option<PathBuf>,
) -> Result<full_sqlite::DatabaseOpen, full_sqlite::InternalError> {
    let (tx, rx) = oneshot::channel();
    let mut rx = rx.fuse();

    let thread_spawn_result = thread::Builder::new().name("database-open".into()).spawn({
        let path = path.clone();
        move || {
            let result = full_sqlite::open(full_sqlite::Config {
                ty: if let Some(path) = &path {
                    full_sqlite::ConfigTy::Disk(path)
                } else {
                    full_sqlite::ConfigTy::Memory
                },
            });
            let _ = tx.send(result);
        }
    });

    // Fall back to opening the database on the same thread if the thread spawn failed.
    if thread_spawn_result.is_err() {
        return full_sqlite::open(full_sqlite::Config {
            ty: if let Some(path) = &path {
                full_sqlite::ConfigTy::Disk(path)
            } else {
                full_sqlite::ConfigTy::Memory
            },
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
