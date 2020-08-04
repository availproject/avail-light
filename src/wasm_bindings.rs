//! Contains `wasm-bindgen` bindings.
//!
//! When this library is compiled for `wasm`, this library contains the types and functions that
//! can be accessed from the user through the `wasm-bindgen` library.

#![cfg(feature = "wasm-bindings")]
#![cfg_attr(docsrs, doc(cfg(feature = "wasm-bindings")))]

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct BrowserLightClient {}

#[wasm_bindgen]
pub async fn start_client(chain_spec: String) -> BrowserLightClient {
    BrowserLightClient {}
}

#[wasm_bindgen]
impl BrowserLightClient {
    /// Starts an RPC request. Returns a `Promise` containing the result of that request.
    #[wasm_bindgen(js_name = "rpcSend")]
    pub fn rpc_send(&mut self, rpc: &str) -> js_sys::Promise {
        wasm_bindgen_futures::future_to_promise(async { todo!() })
    }

    /// Subscribes to an RPC pubsub endpoint.
    #[wasm_bindgen(js_name = "rpcSubscribe")]
    pub fn rpc_subscribe(&mut self, rpc: &str, callback: js_sys::Function) {
        todo!()
    }
}
