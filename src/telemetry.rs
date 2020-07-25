// This file is part of Substrate.

// Copyright (C) 2017-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

// TODO: add docs and clean everything up

use futures::{channel::mpsc, prelude::*};
use libp2p::{wasm_ext, Multiaddr};
use serde::{Deserialize, Deserializer, Serialize};
use std::pin::Pin;

pub use libp2p::wasm_ext::ExtTransport;

pub mod message;
mod worker;

/// Configuration for telemetry.
pub struct TelemetryConfig {
    /// Collection of telemetry WebSocket servers with a corresponding verbosity level.
    pub endpoints: TelemetryEndpoints,

    /// Optional external implementation of a libp2p transport. Used in WASM contexts where we need
    /// some binding between the networking provided by the operating system or environment and
    /// libp2p.
    ///
    /// This parameter exists whatever the target platform is, but it is expected to be set to
    /// `Some` only when compiling for WASM.
    ///
    /// > **Important**: Each individual call to `write` corresponds to one message. There is no
    /// >                internal buffering going on. In the context of WebSockets, each `write`
    /// >                must be one individual WebSockets frame.
    pub wasm_external_transport: Option<wasm_ext::ExtTransport>,

    /// How to send the telemetry background task.
    pub tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
}

/// List of telemetry servers we want to talk to. Contains the URL of the server, and the
/// maximum verbosity level.
///
/// The URL string can be either a URL or a multiaddress.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryEndpoints(
    #[serde(deserialize_with = "url_or_multiaddr_deser")] Vec<(Multiaddr, u8)>,
);

/// Custom deserializer for TelemetryEndpoints, used to convert urls or multiaddr to multiaddr.
fn url_or_multiaddr_deser<'de, D>(deserializer: D) -> Result<Vec<(Multiaddr, u8)>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<(String, u8)>::deserialize(deserializer)?
        .iter()
        .map(|e| {
            Ok((
                url_to_multiaddr(&e.0).map_err(serde::de::Error::custom)?,
                e.1,
            ))
        })
        .collect()
}

impl TelemetryEndpoints {
    pub fn new(endpoints: Vec<(String, u8)>) -> Result<Self, libp2p::multiaddr::Error> {
        let endpoints: Result<Vec<(Multiaddr, u8)>, libp2p::multiaddr::Error> = endpoints
            .iter()
            .map(|e| Ok((url_to_multiaddr(&e.0)?, e.1)))
            .collect();
        endpoints.map(Self)
    }
}

impl TelemetryEndpoints {
    /// Return `true` if there are no telemetry endpoints, `false` otherwise.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Parses a WebSocket URL into a libp2p `Multiaddr`.
fn url_to_multiaddr(url: &str) -> Result<Multiaddr, libp2p::multiaddr::Error> {
    // First, assume that we have a `Multiaddr`.
    let parse_error = match url.parse() {
        Ok(ma) => return Ok(ma),
        Err(err) => err,
    };

    // If not, try the `ws://path/url` format.
    if let Ok(ma) = libp2p::multiaddr::from_url(url) {
        return Ok(ma);
    }

    // If we have no clue about the format of that string, assume that we were expecting a
    // `Multiaddr`.
    Err(parse_error)
}

pub struct Telemetry {
    send: mpsc::Sender<message::TelemetryMessage>,

    events: mpsc::Receiver<TelemetryEvent>,
}

impl Telemetry {
    /// Returns the next event happening on the telemetry.
    pub async fn next_event(&mut self) -> TelemetryEvent {
        if let Some(msg) = self.events.next().await {
            return msg;
        }

        loop {
            futures::pending!()
        }
    }

    /// Sends a telemetry message.
    ///
    /// If we're sending messages at a higher rate than can be processed, messages are silently
    /// discarded.
    pub fn send(&mut self, message: message::TelemetryMessage) {
        let _ = self.send.try_send(message);
    }
}

/// Initializes the telemetry. See the crate root documentation for more information.
pub fn init_telemetry(config: TelemetryConfig) -> Telemetry {
    // Build the list of telemetry endpoints.
    let (endpoints, wasm_external_transport) = (config.endpoints.0, config.wasm_external_transport);

    let (msg_tx, msg_rx) = mpsc::channel(16);
    let (mut events_tx, events_rx) = mpsc::channel(8);

    if let Ok(mut worker) = worker::TelemetryWorker::new(endpoints, wasm_external_transport) {
        let mut msg_rx = msg_rx.fuse();

        (config.tasks_executor)(Box::pin(async move {
            loop {
                futures::select! {
                    msg = msg_rx.next() => {
                        if let Some(msg) = msg {
                            worker.log(msg);
                        } else {
                            return;
                        }
                    }
                    event = future::poll_fn(|cx| worker.poll(cx)).fuse() => {
                        let out = match event {
                            worker::TelemetryWorkerEvent::Connected => TelemetryEvent::Connected,
                        };

                        if events_tx.send(out).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }));
    }

    Telemetry {
        send: msg_tx,
        events: events_rx,
    }
}

/// Event generated when polling the worker.
#[derive(Debug)]
pub enum TelemetryEvent {
    /// We have established a connection to one of the telemetry endpoint, either for the first
    /// time or after having been disconnected earlier.
    Connected,
}
