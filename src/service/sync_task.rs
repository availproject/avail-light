//! Service task that tries to download blocks from the network.

use super::{block_import_task, network_task};
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
    pub to_block_import: mpsc::Sender<block_import_task::ToBlockImport>,
    /// Sender that reports messages to the outside of the service.
    pub to_service_out: mpsc::Sender<super::Event>,
}

/// Runs the sync task.
pub async fn run_sync_task(mut config: Config) {
    let mut next_block = 1 + {
        let (tx, rx) = oneshot::channel();
        config
            .to_block_import
            .send(block_import_task::ToBlockImport::BestBlockNumber { send_back: tx })
            .await
            .unwrap();
        rx.await.unwrap()
    };

    loop {
        let (tx, rx) = oneshot::channel();
        let rq = network::BlocksRequestConfig {
            start: network::BlocksRequestConfigStart::Number(NonZeroU64::new(next_block).unwrap()),
            direction: network::BlocksRequestDirection::Ascending,
            desired_count: 128,
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

        let result = match rx.await.unwrap() {
            Ok(r) => r,
            Err(_) => {
                // TODO: dispatch depending on exact error and remove this wait
                futures_timer::Delay::new(core::time::Duration::from_millis(200)).await;
                continue;
            }
        };

        for block in result {
            let (tx, rx) = oneshot::channel();
            let header = block.header.unwrap();
            let body = block.body.unwrap();
            // TODO: don't unwrap
            let decoded_header =
                <block::Header as parity_scale_codec::DecodeAll>::decode_all(&header.0).unwrap();

            config
                .to_block_import
                .send(block_import_task::ToBlockImport::Import {
                    to_execute: block::Block {
                        header: decoded_header.clone(), // TODO: ideally don't clone? dunno
                        extrinsics: body.into_iter().map(|e| block::Extrinsic(e.0)).collect(),
                    },
                    send_back: tx,
                })
                .await
                .unwrap();

            let success = match rx.await {
                Ok(Ok(s)) => s,
                Ok(Err(_)) => todo!(), // TODO:
                Err(_) => panic!("Import task closed"),
            };

            let result = config
                .to_service_out
                .send(super::Event::NewChainHead {
                    number: decoded_header.number,
                    hash: decoded_header.block_hash().0.into(),
                    head_update: super::ChainHeadUpdate::FastForward, // TODO: dummy
                    modified_keys: success.modified_keys,
                })
                .await;

            // If the channel to the main service is closed, we also shut
            // down the sync task.
            if result.is_err() {
                return;
            }

            next_block += 1;
        }
    }
}
