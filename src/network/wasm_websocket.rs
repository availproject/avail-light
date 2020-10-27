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

// TODO: unfinished

#![cfg(feature = "wasm-bindings")]
#![cfg_attr(docsrs, doc(cfg(feature = "wasm-bindings")))]

use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use wasm_bindgen::JsCast as _;

/// WebSocket client using `wasm-bindgen` for its implementation.
pub struct WebSocketClient {
    inner: web_sys::WebSocket,
    on_open: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>,
    on_close: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::CloseEvent)>,
}

impl WebSocketClient {
    pub async fn connect(url: &str) -> Result<WebSocketClient, ConnectError> {
        let socket = web_sys::WebSocket::new(url).map_err(|_| ConnectError::Unsupported)?;
        socket.set_binary_type(web_sys::BinaryType::Arraybuffer);

        let on_open = wasm_bindgen::closure::Closure::wrap(
            Box::new(|_| {}) as Box<dyn FnMut(web_sys::Event)>
        );
        socket.set_onopen(Some(on_open.as_ref().dyn_ref().unwrap()));

        let on_close = wasm_bindgen::closure::Closure::wrap(
            Box::new(|_| {}) as Box<dyn FnMut(web_sys::CloseEvent)>
        );
        socket.set_onclose(Some(on_close.as_ref().dyn_ref().unwrap()));

        // A manual implementation of `Future` is needed in order to properly clean up the
        // callbacks in case the future is dropped before completion.
        struct Connect {
            inner: Option<web_sys::WebSocket>,
            on_open: Option<wasm_bindgen::closure::Closure<dyn FnMut(web_sys::Event)>>,
            on_close: Option<wasm_bindgen::closure::Closure<dyn FnMut(web_sys::CloseEvent)>>,
        }
        impl Future for Connect {
            type Output = Result<WebSocketClient, ConnectError>;
            fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
                todo!()
            }
        }
        impl Drop for Connect {
            fn drop(&mut self) {
                if let Some(inner) = self.inner.take() {
                    inner.set_onopen(None);
                    inner.set_onclose(None);
                    // Can only return an error if wrong parameters are passed.
                    inner.close().unwrap();
                }
            }
        }

        Connect {
            inner: Some(socket),
            on_open: Some(on_open),
            on_close: Some(on_close),
        }
        .await
    }

    /// Returns the next message received on the WebSocket.
    ///
    /// Returns `None` if the remote sending side has been closed.
    ///
    /// > **Note**: While this method can technically be called multiple times in parallel, from
    /// >           a logic point of view this doesn't make sense, as the ordering of data would
    /// >           be lost.
    pub async fn next_message(&self) -> Option<Vec<u8>> {
        todo!()
    }

    /// Returns the number of bytes that are queued for sending out.
    ///
    /// See [the corresponding MDN documentation](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket/bufferedAmount).
    pub fn buffered_amount(&self) -> u32 {
        self.inner.buffered_amount()
    }

    /// Queues data to send out on the WebSocket connection.
    // TODO: what if already closed?
    pub fn send(&self, data: &[u8]) {
        // Can only result in an error if the WebSocket isn't open anymore.
        self.inner.send_with_u8_array(data).unwrap();
    }

    /// Closes the writing side of the connection.
    ///
    /// Has no effect if the writing side was already closed.
    pub fn close(&self) {
        todo!()
    }
}

#[derive(Debug, derive_more::Display)]
pub enum ConnectError {
    /// Failed to create a JavaScript `WebSocket` object.
    Unsupported,
}
