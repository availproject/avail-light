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

use crate::network;

// TODO: work in progress

use alloc::sync::Arc;
use futures::{
    channel::{mpsc, oneshot},
    lock::Mutex,
    prelude::*,
};
use hashbrown::HashMap;

/// Starts the network service.
pub fn start_network_service(
    worker: network::Network,
) -> (NetworkService, impl Future<Output = ()>) {
    let (tx, rx) = mpsc::channel(8);
    let num_connections_store = Arc::new(atomic::Atomic::new(0));
    let task = run_networking_task(worker, rx, num_connections_store.clone());
    let service = NetworkService {
        sender: Mutex::new(tx),
        num_connections_store,
    };

    (service, task)
}

pub struct NetworkService {
    sender: Mutex<mpsc::Sender<ToWorker>>,
    num_connections_store: Arc<atomic::Atomic<u64>>,
}

impl NetworkService {
    pub async fn block_request(
        &self,
        config: network::BlocksRequestConfig,
    ) -> Result<Vec<network::BlockData>, ()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .lock()
            .await
            .send(ToWorker::BlocksRequest(config, tx))
            .await
            .unwrap();
        rx.await.unwrap()
    }
}

/// Message that can be sent to the network task by the service.
enum ToWorker {
    /// Ask to perform a request from blocks from a peer on the network.
    ///
    /// The received blocks are expected to the same whoever the target of the request is, and
    /// that target is therefore chosen based on some internal logic that distributes the load
    /// amongst the network.
    // TODO: add a variant that requests from a specific target, for fork-aware stuff
    BlocksRequest(
        network::BlocksRequestConfig,
        oneshot::Sender<Result<Vec<network::BlockData>, ()>>,
    ),
}

/// Runs the task.
async fn run_networking_task(
    mut worker: network::Network,
    mut from_service: mpsc::Receiver<ToWorker>,
    num_connections_store: Arc<atomic::Atomic<u64>>,
) {
    // Associates network-assigned block request ids to senders.
    let mut pending_blocks_requests: HashMap<_, oneshot::Sender<_>, fnv::FnvBuildHasher> =
        HashMap::default();

    loop {
        futures::select! {
            ev = worker.next_event().fuse() => {
                match ev {
                    network::Event::BlockAnnounce(header) => {
                        // TODO:
                        /*// TOOD: don't unwrap
                        let decoded_header =
                            <block::Header as parity_scale_codec::DecodeAll>::decode_all(&header.0).unwrap();
                        let ev_out = super::Event::BlockAnnounceReceived {
                            number: decoded_header.number,
                            hash: decoded_header.block_hash().0.into(),
                        };

                        if config.to_service_out.send(ev_out).await.is_err() {
                            return;
                        }*/
                    },
                    network::Event::BlocksRequestFinished { id, result } => {
                        let sender = pending_blocks_requests.remove(&id).unwrap();
                        let _ = sender.send(result);
                    }
                    network::Event::CallRequestFinished { id, result } => {
                        todo!()
                    }
                    network::Event::Connected(peer_id) => {
                        num_connections_store.fetch_add(1, atomic::Ordering::Relaxed);
                    },
                    network::Event::Disconnected(peer_id) => {
                        num_connections_store.fetch_sub(1, atomic::Ordering::Relaxed);
                    },
                }
            }
            ev = from_service.next() => {
                match ev {
                    None => return,
                    Some(ToWorker::BlocksRequest(rq, send_back)) => {
                        if let Ok(id) = worker.start_block_request(rq).await {
                            pending_blocks_requests.insert(id, send_back);
                        } else {
                            let _ = send_back.send(Err(()));
                        }
                    }
                }
            }
        }
    }
}
