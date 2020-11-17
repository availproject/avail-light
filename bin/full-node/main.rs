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

#![recursion_limit = "1024"]
// TODO: add #![deny(unused_crate_dependencies)]

use futures::{channel::oneshot, prelude::*};
use std::{
    borrow::Cow, convert::TryFrom as _, fs, iter, path::PathBuf, sync::Arc, thread, time::Duration,
};
use structopt::StructOpt as _;
use substrate_lite::{
    chain, chain_spec,
    database::full_sled,
    network::{connection, multiaddr, peer_id::PeerId},
};

mod cli;
mod network_service;
mod sync_service;

fn main() {
    futures::executor::block_on(async_main())
}

async fn async_main() {
    let cli_options = cli::CliOptions::from_args();

    let chain_spec = {
        let json: Cow<[u8]> = match cli_options.chain {
            cli::CliChain::Polkadot => (&include_bytes!("../polkadot.json")[..]).into(),
            cli::CliChain::Kusama => (&include_bytes!("../kusama.json")[..]).into(),
            cli::CliChain::Westend => (&include_bytes!("../westend.json")[..]).into(),
            cli::CliChain::Custom(path) => {
                fs::read(&path).expect("Failed to read chain specs").into()
            }
        };

        substrate_lite::chain_spec::ChainSpec::from_json_bytes(&json)
            .expect("Failed to decode chain specs")
    };

    let threads_pool = futures::executor::ThreadPool::builder()
        .name_prefix("tasks-pool-")
        .create()
        .unwrap();

    // Open the database from the filesystem, or create a new database if none is found.
    let database = Arc::new({
        // Directory supposed to contain the database.
        let db_path = {
            let base_path =
                app_dirs::app_dir(app_dirs::AppDataType::UserData, &cli::APP_INFO, "database")
                    .unwrap();
            base_path.join(chain_spec.id())
        };

        // The `unwrap()` here can panic for example in case of access denied.
        match open_database(db_path.clone()).await.unwrap() {
            // Database already exists and contains data.
            full_sled::DatabaseOpen::Open(database) => {
                // TODO: verify that the database matches the chain spec
                // TODO: print the hash in a nicer way
                eprintln!(
                    "Loading existing database with finalized hash {:?}",
                    database.finalized_block_hash().unwrap()
                );
                database
            }

            // The database doesn't exist or is empty.
            full_sled::DatabaseOpen::Empty(empty) => {
                // Build information about state of the chain at the genesis block, and fill the
                // database with it.
                let genesis_chain_information =
                    chain::chain_information::ChainInformation::from_genesis_storage(
                        chain_spec.genesis_storage(),
                    )
                    .unwrap(); // TODO: don't unwrap?

                eprintln!("Initializing new database at {}", db_path.display());

                // The finalized block is the genesis block. As such, it has an empty body and
                // no justification.
                empty
                    .initialize(
                        &genesis_chain_information,
                        iter::empty(),
                        None,
                        chain_spec.genesis_storage(),
                    )
                    .unwrap()
            }
        }
    });

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

    let network_service = network_service::NetworkService::new(network_service::Config {
        listen_addresses: Vec::new(),
        protocol_id: chain_spec.protocol_id().to_owned(),
        genesis_block_hash: database.finalized_block_hash().unwrap(),
        best_block: (0, database.finalized_block_hash().unwrap()),
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
        noise_key: connection::NoiseKey::new(&rand::random()), // TODO: not random
        tasks_executor: {
            let threads_pool = threads_pool.clone();
            Box::new(move |task| threads_pool.spawn_ok(task))
        },
    })
    .await
    .unwrap();

    let sync_service = sync_service::SyncService::new(sync_service::Config {
        tasks_executor: {
            let threads_pool = threads_pool.clone();
            Box::new(move |task| threads_pool.spawn_ok(task))
        },
        database,
    })
    .await;

    /*let mut telemetry = {
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
    };*/

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
                if !cli_options.quiet {
                    // We end the informant line with a `\r` so that it overwrites itself every time.
                    // If any other line gets printed, it will overwrite the informant, and the
                    // informant will then print itself below, which is a fine behaviour.
                    let sync_state = sync_service.sync_state().await;
                    eprint!("{}\r", substrate_lite::informant::InformantLine {
                        enable_colors: match cli_options.color {
                            cli::ColorChoice::Always => true,
                            cli::ColorChoice::Auto => atty::is(atty::Stream::Stderr),
                            cli::ColorChoice::Never => false,
                        },
                        chain_name: chain_spec.name(),
                        max_line_width: terminal_size::terminal_size().map(|(w, _)| w.0.into()).unwrap_or(80),
                        num_network_connections: u64::try_from(network_service.num_established_connections().await)
                            .unwrap_or(u64::max_value()),
                        best_number: sync_state.best_block_number,
                        finalized_number: sync_state.finalized_block_number,
                        best_hash: &sync_state.best_block_hash,
                        finalized_hash: &sync_state.finalized_block_hash,
                        network_known_best: None, /* TODO: match network_state.best_network_block_height.load(Ordering::Relaxed) {
                            0 => None,
                            n => Some(n)
                        },*/
                    });
                }
            },

            network_message = network_service.next_event().fuse() => {
                match network_message {
                    network_service::Event::Connected(peer_id) => {
                        sync_service.add_source(peer_id).await;
                    }
                    network_service::Event::Disconnected(peer_id) => {
                        sync_service.remove_source(peer_id).await;
                    }
                }
            }

            sync_message = sync_service.next_event().fuse() => {
                match sync_message {
                    sync_service::Event::BlocksRequest { id, target, request } => {
                        let block_request = network_service.clone().blocks_request(
                            target,
                            request
                        );

                        threads_pool.spawn_ok({
                            let sync_service = sync_service.clone();
                            async move {
                                let result = block_request.await;
                                sync_service.answer_blocks_request(id, result).await;
                            }
                        });
                    }
                }
            }

            /*telemetry_event = telemetry.next_event().fuse() => {
                telemetry.send(substrate_lite::telemetry::message::TelemetryMessage::SystemConnected(substrate_lite::telemetry::message::SystemConnected {
                    chain: chain_spec.name().to_owned().into_boxed_str(),
                    name: String::from("Polkadot ✨ lite ✨").into_boxed_str(),  // TODO: node name
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
                }));*/
            },
        }
    }
}

/// Since opening the database can take a long time, this utility function performs this operation
/// in the background while showing a small progress bar to the user.
async fn open_database(path: PathBuf) -> Result<full_sled::DatabaseOpen, full_sled::SledError> {
    let (tx, rx) = oneshot::channel();
    let mut rx = rx.fuse();

    let thread_spawn_result = thread::Builder::new().name("database-open".into()).spawn({
        let path = path.clone();
        move || {
            let result = full_sled::open(full_sled::Config { path: &path });
            let _ = tx.send(result);
        }
    });

    // Fall back to opening the database on the same thread if the thread spawn failed.
    if thread_spawn_result.is_err() {
        return full_sled::open(full_sled::Config { path: &path });
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
