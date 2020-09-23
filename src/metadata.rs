// Copyright (C) 2019-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Runtime-provided metadata
//!
//! From the point of the view of the Substrate/Polkadot client, the runtime is a program
//! compiled to WebAssembly that provides a certain list of entry points and has access to a
//! *storage* (provided by the client) as a way to hold information.
//!
//! In order to be able to query an information, for example the amount of tokens present on a
//! certain account, the client can directly read the storage rather than having to enter the
//! WebAssembly code. This is where the *metadata* comes into play.
//!
//! The *metadata* is a collection of data provided by the runtime and that contains useful
//! information to the client, such as:
//!
//! - A list of storage keys whose value contains information that might be useful to the client.
//! - A list of calls that can be performed by emitting transactions.
//! - A list of *events* that can happen in a block, such as a new account.
//! - ...
//!
//! In order to obtain the metadata, a call to an entry point of the runtime code is necessary.
//! Afterwards, the retrieved metadata is guaranteed to not change until the runtime code
//! changes.
//!
//! See also:
//! - https://substrate.dev/docs/en/knowledgebase/runtime/metadata
//!

pub mod decode;
mod query;

pub use query::*;

/// Decodes the given SCALE-encoded metadata.
pub fn decode(scale_encoded_metadata: &[u8]) -> Result<decode::MetadataRef, decode::DecodeError> {
    decode::decode(scale_encoded_metadata)
}

// TODO: functions that generate transactions
// - https://github.com/paritytech/substrate/blob/4cc4b76e361f55de8ae5dd2bae8226cacf4addcb/primitives/runtime/src/generic/unchecked_extrinsic.rs#L38-L48
// - https://github.com/paritytech/substrate-subxt/blob/e85d01ed08e54374d2383e390cd5c2f09b400063/src/extrinsic/mod.rs#L44-L82
// - https://github.com/paritytech/substrate-subxt/blob/e85d01ed08e54374d2383e390cd5c2f09b400063/src/metadata.rs#L190-L196

// TODO: functions that decode events?
// - storage key: https://github.com/paritytech/substrate-subxt/blob/271775bf99092bb890fe8c15eabc87f5b8d3966f/src/rpc.rs#L282-L284
// - decoding this storage entry: https://github.com/paritytech/substrate-subxt/blob/e85d01ed08e54374d2383e390cd5c2f09b400063/src/events.rs#L195-L243
// - in order to decode this storage entry, we need to know the sizes:
//    - https://github.com/paritytech/substrate-subxt/blob/e85d01ed08e54374d2383e390cd5c2f09b400063/src/metadata.rs#L391-L444
//    - https://github.com/paritytech/substrate-subxt/blob/e85d01ed08e54374d2383e390cd5c2f09b400063/src/events.rs#L82-L100
// - this seems overly polkadot-specific and we probably need changes in the runtime before implementing this
