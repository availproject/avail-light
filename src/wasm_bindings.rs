//! Contains `wasm-bindgen` bindings.
//!
//! When this library is compiled for `wasm`, this library contains the types and functions that
//! can be accessed from the user through the `wasm-bindgen` library.

#![cfg(feature = "wasm-bindings")]
#![cfg_attr(docsrs, doc(cfg(feature = "wasm-bindings")))]

use crate::{babe, chain_spec, database, header, network, verify};

use core::{convert::TryFrom as _, num::NonZeroU64};
use futures::prelude::*;
use libp2p::wasm_ext::{ffi, ExtTransport};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct BrowserLightClient {}

#[wasm_bindgen]
pub async fn start_client(chain_spec: String) -> BrowserLightClient {
    // TODO: don't put that here, it's a global setting that doesn't have its place in a library
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));

    // TODO: this entire function is just some temporary code before we figure out where to put it
    // TODO: several places where we unwrap when we shouldn't

    let chain_spec = chain_spec::ChainSpec::from_json_bytes(&chain_spec).unwrap();

    let database = {
        let db_name = format!("substrate-lite-{}", chain_spec.id());
        database::indexed_db_light::Database::open(&db_name)
            .await
            .unwrap()
    };

    let import_queue = {
        let babe_genesis_config = {
            let wasm_code = chain_spec
                .genesis_storage()
                .find(|(k, _)| k == b":code")
                .unwrap()
                .1
                .to_owned();

            babe::BabeGenesisConfiguration::from_runtime_code(&wasm_code, |k| {
                chain_spec
                    .genesis_storage()
                    .find(|(k2, _)| *k2 == k)
                    .map(|(_, v)| v.to_owned())
            })
            .unwrap()
        };

        verify::headers_chain_verify_async::HeadersChainVerifyAsync::new(
            verify::headers_chain_verify_async::Config {
                chain_config: verify::headers_chain_verify::Config {
                    finalized_block_header: crate::calculate_genesis_block_scale_encoded_header(
                        chain_spec.genesis_storage(),
                    ),
                    babe_finalized_block1_slot_number: None, // TODO:
                    babe_known_epoch_information: Vec::new(), // TODO:
                    babe_genesis_config,
                    capacity: 16,
                },
                queue_size: 256,
                tasks_executor: Box::new(|fut| wasm_bindgen_futures::spawn_local(fut)),
            },
        )
    };

    let network = {
        let mut known_addresses = chain_spec
            .boot_nodes()
            .iter()
            .map(|bootnode_str| network::parse_str_addr(bootnode_str).unwrap())
            .collect::<Vec<_>>();
        // TODO: remove this; temporary because bootnode is apparently full
        known_addresses.push((
            "12D3KooWQ9CbBChpa5HH5mADDgwRVdFUGXN3HHsBstbg7Gjhb4ge"
                .parse()
                .unwrap(),
            "/ip4/127.0.0.1/tcp/30333/ws".parse().unwrap(),
        ));

        let worker = network::Network::start(network::Config {
            known_addresses,
            chain_spec_protocol_id: chain_spec.protocol_id().unwrap().as_bytes().to_vec(),
            tasks_executor: Box::new(|fut| wasm_bindgen_futures::spawn_local(fut)),
            local_genesis_hash: crate::calculate_genesis_block_hash(chain_spec.genesis_storage())
                .into(),
            wasm_external_transport: Some(ExtTransport::new(ffi::websocket_transport())),
        })
        .await;

        let (service, future) = network::task::start_network_service(worker);
        wasm_bindgen_futures::spawn_local(future);
        service
    };

    wasm_bindgen_futures::spawn_local(async move {
        let mut next_to_dl = 1;

        loop {
            let blocks = loop {
                let res = network
                    .block_request(network::BlocksRequestConfig {
                        start: network::BlocksRequestConfigStart::Number(
                            NonZeroU64::new(next_to_dl).unwrap(),
                        ),
                        desired_count: u32::max_value(),
                        direction: network::BlocksRequestDirection::Ascending,
                        fields: network::BlocksRequestFields {
                            header: true,
                            body: false,
                            justification: false,
                        },
                    })
                    .await;

                match res {
                    Ok(r) => break r,
                    Err(_) => {
                        wasm_timer::Delay::new(core::time::Duration::from_secs(1))
                            .await
                            .unwrap();
                        continue;
                    }
                }
            };

            next_to_dl += u64::try_from(blocks.len()).unwrap();
            let mut blocks_in_queue = blocks.len();

            for block in blocks {
                import_queue.verify(block.header.unwrap().0, ()).await;
            }

            while blocks_in_queue >= 1 {
                match import_queue.next_event().await {
                    verify::headers_chain_verify_async::Event::VerifyOutcome {
                        scale_encoded_header,
                        result,
                        user_data,
                    } => {
                        blocks_in_queue -= 1;

                        database.insert_header(&scale_encoded_header).await.unwrap();

                        match result {
                            Ok(s) if s.is_new_best => {}
                            _ => continue,
                        };

                        let decoded = header::decode(&scale_encoded_header).unwrap();
                        // TODO: remove this logging
                        if decoded.number % 200 == 0 {
                            web_sys::console::log_1(&JsValue::from_str(&format!(
                                "Imported block #{}!",
                                decoded.number
                            )));
                        }
                    }
                }
            }
        }
    });

    BrowserLightClient {}
}

#[wasm_bindgen]
impl BrowserLightClient {
    /// Starts an RPC request. Returns a `Promise` containing the result of that request.
    #[wasm_bindgen(js_name = "rpcSend")]
    pub fn rpc_send(&mut self, rpc: &str) -> js_sys::Promise {
        wasm_bindgen_futures::future_to_promise(async move {
            // TODO:
            loop {
                futures::pending!()
            }
        })
    }

    /// Subscribes to an RPC pubsub endpoint.
    #[wasm_bindgen(js_name = "rpcSubscribe")]
    pub fn rpc_subscribe(&mut self, rpc: &str, callback: js_sys::Function) {
        // TODO:
    }
}
