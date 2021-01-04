// Substrate-lite
// Copyright (C) 2019-2021  Parity Technologies (UK) Ltd.
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

//! Serializing/deserializing a [`chain_information::ChainInformation`].
//!
//! This module contains the [`encode_chain_information`] and [`decode_chain_information`]
//! functions that can turn a [`chain_information::ChainInformation`] into a string, and back.
//! The string is expected to be in the order of magniture of a few dozens of kilobytes.
//!
//! The string format designed to be stable even if the structure of
//! [`chain_information::ChainInformation`] is later modified.
//!
//! This feature is expected to be used for example by light clients in order to easily (but
//! inefficiently) store the state of the finalized chain somewhere and later reload it.

use crate::chain::chain_information;

use core::convert::TryFrom;

mod defs;

/// Stores the given information in the local storage.
///
/// Errors are expected to be extremely rare, but might happen for example if the serialized
/// data exceeds the browser-specific limit.
pub fn encode_chain_information(information: chain_information::ChainInformationRef<'_>) -> String {
    let decoded = defs::SerializedChainInformation::V1(information.into());
    serde_json::to_string(&decoded).unwrap()
}

/// Loads information about the chain from the local storage.
pub fn decode_chain_information(
    encoded: &str,
) -> Result<chain_information::ChainInformation, CorruptedError> {
    let decoded: defs::SerializedChainInformation = serde_json::from_str(&encoded)
        .map_err(|e| CorruptedError(CorruptedErrorInner::Serde(e)))?;

    match decoded {
        defs::SerializedChainInformation::V1(decoded) => Ok(TryFrom::try_from(decoded)
            .map_err(|err| CorruptedError(CorruptedErrorInner::Deserialize(err)))?),
    }
}

/// Opaque error indicating a corruption in the data stored in the local storage.
#[derive(Debug, derive_more::Display)]
#[display(fmt = "{}", _0)]
pub struct CorruptedError(CorruptedErrorInner);

#[derive(Debug, derive_more::Display)]
enum CorruptedErrorInner {
    #[display(fmt = "{}", _0)]
    Serde(serde_json::Error),
    #[display(fmt = "{}", _0)]
    Deserialize(defs::DeserializeError),
}
