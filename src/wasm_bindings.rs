//! Contains `wasm-bindgen` bindings.
//!
//! When this library is compiled for `wasm`, this library contains the types and functions that
//! can be accessed from the user through the `wasm-bindgen` library.

#![cfg(feature = "wasm-bindings")]
#![cfg_attr(docsrs, doc(cfg(feature = "wasm-bindings")))]

use crate::{chain_spec, network};

use libp2p::wasm_ext::{ExtTransport, ffi};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct BrowserLightClient {}

#[wasm_bindgen]
pub async fn start_client(chain_spec: String) -> BrowserLightClient {
    // TODO: don't put that here, it's a global setting that doesn't have its place in a library
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));

    // TODO: this is just some temporary code before we figure out where to put it
    let chain_spec = chain_spec::ChainSpec::from_json_bytes(&chain_spec).unwrap();

    // TODO: let database = crate::database::indexed_db_light::open().await;

    wasm_bindgen_futures::spawn_local({
        let mut network = network::Network::start(network::Config {
            known_addresses: chain_spec
                .boot_nodes()
                .iter()
                .map(|bootnode_str| network::parse_str_addr(bootnode_str).unwrap())
                .collect(),
            chain_spec_protocol_id: chain_spec.protocol_id().unwrap().as_bytes().to_vec(),
            tasks_executor: Box::new(|fut| wasm_bindgen_futures::spawn_local(fut)),
            local_genesis_hash: crate::calculate_genesis_block_hash(chain_spec.genesis_storage()).into(),
            wasm_external_transport: Some(ExtTransport::new(ffi::websocket_transport())),
        }).await;

        async move {
            loop {
                match network.next_event().await {
                    ev => {
                        web_sys::console::log_1(&JsValue::from_str(&format!("{:?}", ev)));
                    },
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
