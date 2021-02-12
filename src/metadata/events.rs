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

//! Events retrieval and decoding.
//!
//! # Overview
//!
//! While this behaviour is not part of the specifications and is thus not a strict requirement,
//! Substrate-compatible blockchains built using the Substrate framework provide a storage item
//! (in other words, an entry in the storage, with a specific key) containing a list of so-called
//! *events*. This storage item is updated at each block with the events that have happened in
//! the latest block.
//!
//! > **Note**: Events include, for example, a transfer between two accounts, a change in the
//! >           validators nominated by a nominator, the beginning of a referundum, etc.
//!
//! This module provides the tooling necessary to help retrieve said events.
//!
//! In order to determine which storage key holds the list of events, one needs to provide the
//! *metadata*. See the [metadata](crate::metadata) module for information about what the metadata
//! is and how to obtain it.
//!
//! # Usage
//!
//! In order to know the events that have happened during a specific block:
//!
//! - Obtain the *metadata* of the runtime used by the desired block. This is out of scope of this
//! module. See the [metadata](crate::metadata) module for more information.
//! - Call [`events_storage_key`] in order to obtain a key where to find the list of events in the
//! storage. If [`events_storage_key`] returns an error, the runtime most likely doesn't support
//! events.
//! - Obtain the storage value corresponding to the key obtained at the previous step. This is out
//! of scope of this module. If there is no storage value at this key, this most likely indicates
//! a bug somewhere, either in substrate-lite or in the runtime.
//! - Decode the events. This isn't implemented in this module yet. See the next section.
//!
//! # Flaw in the design
//!
//! In the SCALE codec, each field is put one behind the other in an unstructured way. The start
//! of a new field is known by adding the length of all the preceding fields, and the length of
//! a field depends on the type of data it contains. The type of data is not explicitly laid out
//! and is only known through context.
//!
//! The list of events (encoded using SCALE) contains, amongst other things, the values of the
//! parameters of said event. In order to be able decode this list of events, one has to know the
//! types of these parameter values. This information is found in the metadata, in the form of a
//! string representing the type as written out in the original Rust source code. This type could
//! be anything, from a primitive type (e.g. `u32`) to a type alias (`type Foo = ...;`), or a
//! locally-defined struct.
//!
//! In order to properly decode events, one has to parse these strings representing Rust types
//! and hard-code a list of types known to be used in the runtime code. This ranges from `u32` to
//! for example `EthereumAddress`.
//!
//! Runtime upgrades can introduce new types and (albeit unlikely) modify the definition of
//! existing types. As such, a function that decodes events can stop working after any runtime
//! upgrade. For this reason, smoldot doesn't provide any such function.
//!
//! In the future, it is planned to add, in Substrate, runtime type information to the metadata,
//! thus allowing a proper decoding function to be implemented. When that is done, smoldot can be
//! updated to support decoding events.
//!

use crate::metadata::decode as metadata;
use core::{convert::TryFrom, hash::Hasher as _};

/// Returns the key in the storage at which events can be found.
///
/// > **Note**: This key is based entirely on the metadata passed as parameter. Be aware that,
/// >           albeit unlikely, if the metadata changes, the key might change as well.
///
/// An error is returned if the metadata doesn't indicate any storage entry for events, or if the
/// type of the content of the storage entry isn't recognized.
pub fn events_storage_key(
    mut metadata: metadata::MetadataRef,
) -> Result<[u8; 32], EventsStorageKeyError> {
    let module = metadata
        .modules
        .find(|m| m.name == "System")
        .ok_or(EventsStorageKeyError::NoSystemModule)?;

    let mut storage = module.storage.ok_or(EventsStorageKeyError::NoEventsKey)?;

    let entry = storage
        .entries
        .find(|e| e.name == "Events")
        .ok_or(EventsStorageKeyError::NoEventsKey)?;
    if entry.ty != metadata::StorageEntryTypeRef::Plain("Vec<EventRecord<T::Event, T::Hash>>") {
        return Err(EventsStorageKeyError::WrongType);
    }

    let mut out = [0; 32];
    twox_128(
        storage.prefix.as_bytes(),
        TryFrom::try_from(&mut out[..16]).unwrap(),
    );
    twox_128(
        entry.name.as_bytes(),
        TryFrom::try_from(&mut out[16..]).unwrap(),
    );
    Ok(out)
}

/// Error potentially returned by [`events_storage_key`].
#[derive(Debug, derive_more::Display)]
pub enum EventsStorageKeyError {
    /// No module called `System` has been found.
    NoSystemModule,
    /// No storage entry called `Events` has been found.
    NoEventsKey,
    /// The `Events` storage key doesn't have the type expected for a list of events.
    WrongType,
}

/// Fills `dest` with the XXHash of `data`.
fn twox_128(data: &[u8], dest: &mut [u8; 16]) {
    let mut h0 = twox_hash::XxHash::with_seed(0);
    let mut h1 = twox_hash::XxHash::with_seed(1);
    h0.write(&data);
    h1.write(&data);
    let r0 = h0.finish();
    let r1 = h1.finish();

    dest[..8].copy_from_slice(&r0.to_le_bytes()[..]);
    dest[8..].copy_from_slice(&r1.to_le_bytes()[..]);
}
