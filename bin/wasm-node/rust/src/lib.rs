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

//! Contains a light client implementation usable from a browser environment, using the
//! `wasm-bindgen` library.

#![recursion_limit = "512"]
#![deny(broken_intra_doc_links)]
#![deny(unused_crate_dependencies)]

use futures::{channel::mpsc, lock::Mutex, prelude::*};
use smoldot::{
    chain, chain_spec,
    libp2p::{multiaddr, peer_id::PeerId},
};
use std::{collections::HashMap, pin::Pin, sync::Arc, time::Duration};

pub mod ffi;

mod json_rpc_service;
mod lossy_channel;
mod network_service;
mod runtime_service;
mod sync_service;
mod transactions_service;

// Use the default "system" allocator. In the context of Wasm, this uses the `dlmalloc` library.
// See <https://github.com/rust-lang/rust/tree/1.47.0/library/std/src/sys/wasm>.
//
// While the `wee_alloc` crate is usually the recommended choice in WebAssembly, testing has shown
// that using it makes memory usage explode from ~100MiB to ~2GiB and more (the environment then
// refuses to allocate 4GiB).
#[global_allocator]
static ALLOC: std::alloc::System = std::alloc::System;

pub struct ChainConfig {
    pub specification: String,
    pub json_rpc_running: bool,
}

/// Starts a client running the given chain specifications.
pub async fn start_client(
    chains: impl Iterator<Item = ChainConfig>,
    max_log_level: log::LevelFilter,
) {
    // Try initialize the logging and the panic hook.
    // Note that `start_client` can theoretically be called multiple times, meaning that these
    // calls shouldn't panic if reached multiple times.
    let _ =
        log::set_boxed_logger(Box::new(ffi::Logger)).map(|()| log::set_max_level(max_log_level));
    std::panic::set_hook(Box::new(|info| {
        ffi::throw(info.to_string());
    }));

    // Fool-proof check to make sure that randomness is properly implemented.
    assert_ne!(rand::random::<u64>(), 0);
    assert_ne!(rand::random::<u64>(), rand::random::<u64>());

    // Decode the chain specifications, and whether the chain should be running a JSON-RPC service.
    let (chain_specs, json_rpc_running) = {
        let mut chain_specs = Vec::new();
        let mut json_rpc_running = Vec::new();

        for chain in chains {
            chain_specs.push(
                match chain_spec::ChainSpec::from_json_bytes(&chain.specification) {
                    Ok(cs) => {
                        log::info!("Loaded chain specs for {}", cs.name());
                        cs
                    }
                    Err(err) => ffi::throw(format!("Error while opening chain specs: {}", err)),
                },
            );

            json_rpc_running.push(chain.json_rpc_running);
        }

        (chain_specs, json_rpc_running)
    };

    // Load the information about the chains from the chain specs. If a light sync state is
    // present in the chain specs, it is possible to start sync at the finalized block it
    // describes.
    let genesis_chain_information = chain_specs
        .iter()
        .map(|chain_spec| {
            chain::chain_information::ChainInformation::from_chain_spec(&chain_spec).unwrap()
        })
        .collect::<Vec<_>>();
    let chain_information = chain_specs
        .iter()
        .zip(genesis_chain_information.iter())
        .map(|(chain_spec, genesis_chain_information)| {
            if let Some(light_sync_state) = chain_spec.light_sync_state() {
                log::info!(
                    "Using light checkpoint starting at #{}",
                    light_sync_state
                        .as_chain_information()
                        .finalized_block_header
                        .number
                );
                light_sync_state.as_chain_information()
            } else {
                genesis_chain_information.clone()
            }
        })
        .collect::<Vec<_>>();

    // Starting here, the code below initializes the various "services" that make up the node.
    // Services need to be able to spawn asynchronous tasks on their own. Since "spawning a task"
    // isn't really something that a browser or Node environment can do efficiently, we instead
    // combine all the asynchronous tasks into one `FuturesUnordered` below.
    //
    // The `new_task_tx` and `new_task_rx` variables are used when spawning a new task is
    // required. Send a task on `new_task_tx` to start running it.
    let (new_task_tx, mut new_task_rx) = mpsc::unbounded();

    // The code below consists in spawning various services one by one. Services must be created
    // in a specific order, because some services must be passed an `Arc` to others.
    // One thing to be aware of, is that in order to start, a service might perform a request on
    // the other service(s) passed as parameter. These requests might in turn depend on background
    // tasks being spawned. For this reason, the tasks sent to `new_task_tx` must be start running
    // as soon as possible, and not be delayed.
    // This is why we spawn the entire initialization through `new_task_tx` instead of directly
    // using `await`.
    new_task_tx
        .clone()
        .unbounded_send(
            start_services(
                new_task_tx,
                chain_information,
                genesis_chain_information,
                chain_specs,
                json_rpc_running,
            )
            .boxed(),
        )
        .unwrap();

    // This is the main future that executes the entire client.
    let mut all_tasks = stream::FuturesUnordered::new();
    async move {
        // Since `all_tasks` is initially empty, polling it would produce `None` and immediately
        // interrupt the processing.
        // As such, we start by filling it with the initial content of the `new_task` channel.
        while let Some(Some(task)) = new_task_rx.next().now_or_never() {
            all_tasks.push(task);
        }

        loop {
            match future::select(new_task_rx.select_next_some(), all_tasks.next()).await {
                future::Either::Left((new_task, _)) => {
                    all_tasks.push(new_task);
                }
                future::Either::Right((Some(()), _)) => {}
                future::Either::Right((None, _)) => {
                    log::info!("All tasks complete. Stopping client.");
                    break;
                }
            }
        }
    }
    .await
}

/// Starts all the services of the client.
async fn start_services(
    new_task_tx: mpsc::UnboundedSender<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>,
    chain_information: Vec<chain::chain_information::ChainInformation>,
    genesis_chain_information: Vec<chain::chain_information::ChainInformation>,
    chain_specs: Vec<chain_spec::ChainSpec>,
    json_rpc_running: Vec<bool>,
) {
    // The network service is responsible for connecting to the peer-to-peer network
    // of all chains.
    let (network_service, mut network_event_receivers) =
        network_service::NetworkService::new(network_service::Config {
            tasks_executor: Box::new({
                let new_task_tx = new_task_tx.clone();
                move |fut| new_task_tx.unbounded_send(fut).unwrap()
            }),
            num_events_receivers: chain_information.len(), // Configures the length of `network_event_receivers`
            chains: chain_information
                .iter()
                .zip(chain_specs.iter())
                .zip(genesis_chain_information.iter())
                .map(
                    |((chain_information, chain_spec), genesis_chain_information)| {
                        network_service::ConfigChain {
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
                            has_grandpa_protocol: matches!(
                                genesis_chain_information.finality,
                                chain::chain_information::ChainInformationFinality::Grandpa { .. }
                            ),
                            genesis_block_hash: genesis_chain_information
                                .finalized_block_header
                                .hash(),
                            best_block: (
                                chain_information.finalized_block_header.number,
                                chain_information.finalized_block_header.hash(),
                            ),
                            protocol_id: chain_spec.protocol_id().to_string(),
                        }
                    },
                )
                .collect(),
        })
        .await;

    // The network service is the only one in common between all chains. Other services run once
    // per chain.

    // The `Vec` below is filled when we start the services of a chain.
    let mut per_chain: Vec<
        Option<(
            Arc<sync_service::SyncService>,
            Arc<runtime_service::RuntimeService>,
        )>,
    > = (0..chain_specs.len()).map(|_| None).collect();

    // Start the services of the chains that aren't parachains.
    for (
        relay_chain_index,
        (chain_index, ((chain_information, chain_spec), genesis_chain_information)),
    ) in chain_information
        .iter()
        .zip(chain_specs.iter())
        .zip(genesis_chain_information.iter())
        .enumerate()
        .filter(|(_, ((_, chain_spec), _))| chain_spec.relay_chain().is_none())
        .enumerate()
    {
        // The sync service is leveraging the network service, downloads block headers,
        // and verifies them, to determine what are the best and finalized blocks of the
        // chain.
        let sync_service = Arc::new(
            sync_service::SyncService::new(sync_service::Config {
                chain_information: chain_information.clone(),
                tasks_executor: Box::new({
                    let new_task_tx = new_task_tx.clone();
                    move |fut| new_task_tx.unbounded_send(fut).unwrap()
                }),
                network_service: (network_service.clone(), chain_index),
                network_events_receiver: network_event_receivers.pop().unwrap(),
                parachain: None,
            })
            .await,
        );

        // The runtime service follows the runtime of the best block of the chain,
        // and allows performing runtime calls.
        let runtime_service = runtime_service::RuntimeService::new(runtime_service::Config {
            tasks_executor: Box::new({
                let new_task_tx = new_task_tx.clone();
                move |fut| new_task_tx.unbounded_send(fut).unwrap()
            }),
            network_service: (network_service.clone(), chain_index),
            sync_service: sync_service.clone(),
            chain_spec: &chain_spec,
            genesis_block_hash: genesis_chain_information.finalized_block_header.hash(),
            genesis_block_state_root: genesis_chain_information.finalized_block_header.state_root,
        })
        .await;

        debug_assert!(per_chain[chain_index].is_none());
        per_chain[chain_index] = Some((sync_service.clone(), runtime_service));
    }

    // Start the services of the parachains.
    for (chain_index, ((chain_information, chain_spec), genesis_chain_information)) in
        chain_information
            .iter()
            .zip(chain_specs.iter())
            .zip(genesis_chain_information.iter())
            .enumerate()
    {
        // Skip non-parachains.
        let (relay_chain_id, parachain_id) = match chain_spec.relay_chain() {
            Some(v) => v,
            None => continue,
        };

        // Find the index of the relay chain in the list of chains.
        let relay_chain_index = match chain_specs.iter().position(|s| s.id() == relay_chain_id) {
            Some(idx) => idx,
            None => panic!("Couldn't find relay chain `{}`", relay_chain_id),
        };

        // Note that at the time of writing, due to the structure of this code, relay chains that
        // are themselves parachains would only work if the relay chain is before the parachain
        // in the list. No existing live chain is in this situation at the time of writing of this
        // comment.
        let relay_chain_services = match per_chain[relay_chain_index].as_ref() {
            Some(s) => s,
            None => panic!(
                "Relay chain `{}` is itself a parachain. This isn't supported by smoldot yet.",
                relay_chain_id
            ),
        };

        // The sync service is leveraging the network service, downloads block headers,
        // and verifies them, to determine what are the best and finalized blocks of the
        // chain.
        let sync_service = Arc::new(
            sync_service::SyncService::new(sync_service::Config {
                chain_information: chain_information.clone(),
                tasks_executor: Box::new({
                    let new_task_tx = new_task_tx.clone();
                    move |fut| new_task_tx.unbounded_send(fut).unwrap()
                }),
                network_service: (network_service.clone(), chain_index),
                network_events_receiver: network_event_receivers.pop().unwrap(),
                parachain: Some(sync_service::ConfigParachain {
                    parachain_id,
                    relay_chain_sync: relay_chain_services.1.clone(),
                    relay_network_chain_index: relay_chain_index,
                }),
            })
            .await,
        );

        // The runtime service follows the runtime of the best block of the chain,
        // and allows performing runtime calls.
        let runtime_service = runtime_service::RuntimeService::new(runtime_service::Config {
            tasks_executor: Box::new({
                let new_task_tx = new_task_tx.clone();
                move |fut| new_task_tx.unbounded_send(fut).unwrap()
            }),
            network_service: (network_service.clone(), chain_index),
            sync_service: sync_service.clone(),
            chain_spec,
            genesis_block_hash: genesis_chain_information.finalized_block_header.hash(),
            genesis_block_state_root: genesis_chain_information.finalized_block_header.state_root,
        })
        .await;

        debug_assert!(per_chain[chain_index].is_none());
        per_chain[chain_index] = Some((sync_service.clone(), runtime_service));
    }

    debug_assert!(per_chain.iter().all(Option::is_some));

    // Spawn the JSON-RPC services. They are responsible for answering incoming JSON-RPC requests.
    let mut json_rpc_services = HashMap::new();
    for (chain_index, (((services, json_rpc_running), genesis_chain_information), chain_spec)) in
        per_chain
            .into_iter()
            .zip(json_rpc_running)
            .zip(genesis_chain_information)
            .zip(chain_specs)
            .enumerate()
    {
        if !json_rpc_running {
            continue;
        }

        let (sync_service, runtime_service) = services.unwrap();

        let finalized_header = genesis_chain_information.finalized_block_header;
        let transactions_service = Arc::new(
            transactions_service::TransactionsService::new(transactions_service::Config {
                tasks_executor: Box::new({
                    let new_task_tx = new_task_tx.clone();
                    move |fut| new_task_tx.unbounded_send(fut).unwrap()
                }),
                network_service: (network_service.clone(), 0),
                sync_service: sync_service.clone(),
            })
            .await,
        );

        let json_rpc_service = json_rpc_service::start(json_rpc_service::Config {
            tasks_executor: Box::new({
                let new_task_tx = new_task_tx.clone();
                move |fut| new_task_tx.unbounded_send(fut).unwrap()
            }),
            network_service: (network_service.clone(), 0),
            sync_service,
            transactions_service,
            runtime_service,
            chain_spec,
            genesis_block_hash: finalized_header.hash(),
            genesis_block_state_root: finalized_header.state_root,
            chain_index,
        })
        .await;

        json_rpc_services.insert(chain_index, json_rpc_service);
    }

    new_task_tx
        .unbounded_send(
            json_rpc_service::request_handling_task(
                Arc::new(Mutex::new(Box::new({
                    let new_task_tx = new_task_tx.clone();
                    move |fut| new_task_tx.unbounded_send(fut).unwrap()
                }))),
                json_rpc_services,
            )
            .boxed(),
        )
        .unwrap();

    log::info!("Initialization complete");
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
