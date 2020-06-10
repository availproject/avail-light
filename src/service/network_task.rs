//! Contains code required to plug the networking together with the rest of the service.
//!
//! Contrary to the [crate::network] module, this module is aware of the other tasks of the
//! service.

use crate::{block, network};

use alloc::sync::Arc;
use core::sync::atomic;
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use hashbrown::HashMap;

/// Message that can be sent to the network task by the other parts of the code.
pub enum ToNetwork {
    BlocksRequest(
        network::BlocksRequestConfig,
        oneshot::Sender<Result<Vec<network::BlockData>, ()>>,
    ),
}

/// Configuration for that task.
pub struct Config {
    /// Prototype for the network worker.
    pub network_builder: network::builder::NetworkBuilder,
    /// Sender that reports messages to the outside of the service.
    pub to_service_out: mpsc::Sender<super::Event>,
    /// Receiver to receive messages that the networking task will process.
    pub to_network: mpsc::Receiver<super::network_task::ToNetwork>,
    /// `Arc` where to store the number of transport-level connections. Incremented whenever we
    /// open a new transport-level connection and decremented whenever we close one.
    pub num_connections_store: Arc<atomic::AtomicU64>,
}

/// Runs the task.
pub async fn run_networking_task(mut config: Config) {
    let mut network = config.network_builder.build().await;

    // Associates network-assigned block request ids to senders.
    let mut pending_blocks_requests = HashMap::<_, oneshot::Sender<_>>::new();

    loop {
        futures::select! {
            ev = network.next_event().fuse() => {
                match ev {
                    network::Event::BlockAnnounce(header) => {
                        // TOOD: don't unwrap
                        let decoded_header =
                            <block::Header as parity_scale_codec::DecodeAll>::decode_all(&header.0).unwrap();
                        let ev_out = super::Event::BlockAnnounceReceived {
                            number: decoded_header.number,
                            hash: decoded_header.block_hash().0.into(),
                        };

                        if config.to_service_out.send(ev_out).await.is_err() {
                            return;
                        }
                    },
                    network::Event::BlocksRequestFinished { id, result } => {
                        let sender = pending_blocks_requests.remove(&id).unwrap();
                        let _ = sender.send(result);
                    }
                    network::Event::Connected(peer_id) => {
                        config.num_connections_store.fetch_add(1, atomic::Ordering::Relaxed);
                    },
                    network::Event::Disconnected(peer_id) => {
                        config.num_connections_store.fetch_sub(1, atomic::Ordering::Relaxed);
                    },
                    // TODO: send out `service::Event::NewNetworkExternalAddress`
                }
            }
            ev = config.to_network.next() => {
                match ev {
                    None => return,
                    Some(ToNetwork::BlocksRequest(rq, send_back)) => {
                        let id = network.start_block_request(rq).await;
                        pending_blocks_requests.insert(id, send_back);
                    }
                }
            }
        }
    }
}
