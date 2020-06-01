//! Service task that tries to download blocks from the network.

use super::{executor_task, network_task};
use crate::{block, network};

use core::num::NonZeroU64;
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};

/// Configuration for that task.
pub struct Config {
    /// Channel to send instructions to the networking task.
    pub to_network: mpsc::Sender<network_task::ToNetwork>,
    /// Channel to send instructions to the executor task.
    pub to_executor: mpsc::Sender<executor_task::ToExecutor>,
}

/// Runs the sync task.
pub async fn run_sync_task(mut config: Config) {
    // TODO: we cheat, to wait to be connected to nodes
    futures_timer::Delay::new(core::time::Duration::from_secs(5)).await;

    let (tx, rx) = oneshot::channel();
    let rq = network::BlocksRequestConfig {
        start: network::BlocksRequestConfigStart::Number(NonZeroU64::new(1).unwrap()),
        direction: network::BlocksRequestDirection::Ascending,
        desired_count: 1,
        fields: network::BlocksRequestFields {
            header: true,
            body: true,
            justification: false,
        },
    };
    config
        .to_network
        .send(network_task::ToNetwork::BlocksRequest(rq, tx))
        .await
        .unwrap();
    let result = rx.await.unwrap().unwrap();

    for block in result {
        let (tx, _rx) = oneshot::channel();
        let header = block.header.unwrap();
        let body = block.body.unwrap();
        config
            .to_executor
            .send(executor_task::ToExecutor::Execute {
                to_execute: block::Block {
                    header: block::Header {
                        parent_hash: header.parent_hash,
                        number: header.number,
                        state_root: header.state_root,
                        extrinsics_root: header.extrinsics_root,
                        digest: block::Digest {
                            logs: Vec::new(), // TODO:
                        },
                    },
                    extrinsics: body.into_iter().map(|e| block::Extrinsic(e.0)).collect(),
                },
                send_back: tx,
            })
            .await
            .unwrap();
    }

    // TODO: remove that; hack to not make the task stop
    loop {
        futures::pending!()
    }
}
