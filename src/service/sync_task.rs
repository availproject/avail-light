//! Service task that tries to download blocks from the network.

use super::{block_import_task, network_task};
use crate::{block, network};

use alloc::collections::VecDeque;
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
    // Number of the block at the head of the chain.
    let mut head_of_chain = {
        let (tx, rx) = oneshot::channel();
        config
            .to_block_import
            .send(block_import_task::ToBlockImport::BestBlockNumber { send_back: tx })
            .await
            .unwrap();
        rx.await.unwrap()
    };

    // Number of the next block to download from the peer-to-peer network.
    let mut next_to_download = 1 + head_of_chain;

    // Collection of futures that yield blocks from the network.
    // TODO: FuturesOrdered really should implement FusedStream
    let mut pending_downloads = stream::FuturesOrdered::new().fuse();

    // Collection of futures that report about import results.
    // TODO: FuturesOrdered really should implement FusedStream
    let mut pending_imports = stream::FuturesOrdered::new().fuse();

    // Queue of blocks that have been downloaded and waiting to be submitted to the import queue.
    let mut blocks_queue = VecDeque::<network::BlockData>::with_capacity(2048);

    loop {
        // Download more blocks if appropriate.
        // TODO: fails if more than one download at a time because it's not properly implement in the network thing
        while pending_downloads.get_ref().len() <= 0 && blocks_queue.len() <= 4096 {
            let (tx, rx) = oneshot::channel();
            let rq = network::BlocksRequestConfig {
                start: network::BlocksRequestConfigStart::Number(
                    NonZeroU64::new(next_to_download).unwrap(),
                ),
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

            pending_downloads
                .get_mut()
                .push(rx.map(move |rp| (rp, next_to_download)));
            pending_downloads = pending_downloads.into_inner().fuse(); // TODO: hack around the fuse stupidity
            next_to_download += 128;
        }

        // Try submit as many blocks from the queue as possible.
        while future::poll_fn(|cx| config.to_block_import.poll_ready(cx))
            .now_or_never()
            .is_some()
        {
            let block = match blocks_queue.pop_front() {
                Some(b) => b,
                None => break,
            };

            let (tx, rx) = oneshot::channel();
            let header = block.header.unwrap();
            let body = block.body.unwrap();
            // TODO: don't unwrap
            let decoded_header =
                <block::Header as parity_scale_codec::DecodeAll>::decode_all(&header.0).unwrap();

            config
                .to_block_import
                .start_send(block_import_task::ToBlockImport::Import {
                    to_execute: block::Block {
                        header: decoded_header.clone(), // TODO: ideally don't clone? dunno
                        extrinsics: body.into_iter().map(|e| block::Extrinsic(e.0)).collect(),
                    },
                    send_back: tx,
                })
                .unwrap();

            pending_imports.get_mut().push(rx);
            pending_imports = pending_imports.into_inner().fuse(); // TODO: hack around the fuse stupidity
        }

        // Make sure the `select!` below doesn't block forever.
        // TODO: can panic if something else submits blocks to the block_import queue; is this important?
        assert!(!pending_downloads.get_ref().is_empty() || !pending_imports.get_ref().is_empty());

        futures::select! {
            (download_result, block_num) = pending_downloads.select_next_some() => {
                match download_result {
                    Ok(Ok(blocks)) => {
                        for block in blocks {
                            blocks_queue.push_back(block);
                        }
                    },
                    Ok(Err(_)) => {
                        // TODO: only try again the failed request, but that's efforts
                        // TODO: this doesn't actually stop the requests
                        pending_downloads = stream::FuturesOrdered::new().fuse();
                        next_to_download = block_num;

                        // TODO: dispatch depending on exact error and remove this wait
                        futures_timer::Delay::new(core::time::Duration::from_millis(2000)).await;
                        continue;
                    }
                    Err(_) => panic!("Download task closed"),
                }
            }

            import_result = pending_imports.select_next_some() => {
                let success = match import_result {
                    Ok(Ok(s)) => s,
                    Ok(Err(err)) => todo!("block import error: {:?}", err), // TODO:
                    Err(_) => panic!("Import task closed"),
                };

                head_of_chain += 1;
                assert_eq!(head_of_chain, success.block.header.number);

                let result = config
                    .to_service_out
                    .send(super::Event::NewChainHead {
                        number: success.block.header.number,
                        hash: success.block.block_hash().0.into(),
                        head_update: super::ChainHeadUpdate::FastForward, // TODO: dummy
                        modified_keys: success.modified_keys,
                    })
                    .await;

                // If the channel to the main service is closed, we also shut
                // down the sync task.
                if result.is_err() {
                    return;
                }
            }
        }
    }
}
