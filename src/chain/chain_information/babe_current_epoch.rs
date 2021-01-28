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

use crate::{
    chain::chain_information::BabeEpochInformation,
    executor::{host, read_only_runtime_host},
    header,
};

use alloc::vec::Vec;
use parity_scale_codec::{Decode, Encode};

/// Configuration for [`babe_current_epoch`].
pub struct Config {
    /// Runtime used to get the current babe epoch. Must be built using the Wasm code found at the
    /// `:code` key of the block storage.
    pub runtime: host::HostVmPrototype,
}

/// Problem encountered during a call to [`babe_current_epoch`].
#[derive(Debug, Clone, derive_more::Display)]
pub enum Error {
    /// Error while starting the Wasm virtual machine.
    #[display(fmt = "{}", _0)]
    WasmStart(host::StartErr),
    /// Error while running the Wasm virtual machine.
    #[display(fmt = "{}", _0)]
    WasmVm(read_only_runtime_host::Error),
    /// Error while decoding the babe epoch.
    #[display(fmt = "{}", _0)]
    DecodeFailed(parity_scale_codec::Error),
}

/// Fetches the current Babe epoch using `BabeApi_current_epoch`.
pub fn babe_current_epoch(config: Config) -> Query {
    let vm = read_only_runtime_host::run(read_only_runtime_host::Config {
        virtual_machine: config.runtime,
        function_to_call: "BabeApi_current_epoch",
        // `BabeApi_current_epoch` doesn't take any parameters.
        parameter: core::iter::empty::<&[u8]>(),
    });

    match vm {
        Ok(vm) => Query::from_inner(vm),
        Err(err) => Query::Finished(Err(Error::WasmStart(err))),
    }
}

/// Current state of the operation.
#[must_use]
pub enum Query {
    /// Fetching the Babe epoch is over.
    Finished(Result<(BabeEpochInformation, host::HostVmPrototype), Error>),
    /// Loading a storage value is required in order to continue.
    StorageGet(StorageGet),
    /// Fetching the key that follows a given one is required in order to continue.
    NextKey(NextKey),
}

impl Query {
    fn from_inner(inner: read_only_runtime_host::RuntimeHostVm) -> Self {
        match inner {
            read_only_runtime_host::RuntimeHostVm::Finished(Ok(success)) => {
                let mut value = success.virtual_machine.value();

                match DecodableBabeEpochInformation::decode(&mut value) {
                    Ok(epoch) => Query::Finished(Ok((
                        BabeEpochInformation {
                            epoch_index: epoch.epoch_index,
                            start_slot_number: epoch.start_slot_number,
                            authorities: epoch
                                .authorities
                                .into_iter()
                                .map(|authority| header::BabeAuthority {
                                    public_key: authority.public_key,
                                    weight: authority.weight,
                                })
                                .collect(),
                            randomness: epoch.randomness,
                            c: epoch.c,
                            allowed_slots: epoch.allowed_slots,
                        },
                        success.virtual_machine.into_prototype(),
                    ))),
                    Err(error) => Query::Finished(Err(Error::DecodeFailed(error))),
                }
            }
            read_only_runtime_host::RuntimeHostVm::Finished(Err(err)) => {
                Query::Finished(Err(Error::WasmVm(err)))
            }
            read_only_runtime_host::RuntimeHostVm::StorageGet(inner) => {
                Query::StorageGet(StorageGet(inner))
            }
            read_only_runtime_host::RuntimeHostVm::NextKey(inner) => Query::NextKey(NextKey(inner)),
        }
    }
}

/// Loading a storage value is required in order to continue.
#[must_use]
pub struct StorageGet(read_only_runtime_host::StorageGet);

impl StorageGet {
    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    pub fn key<'a>(&'a self) -> impl Iterator<Item = impl AsRef<[u8]> + 'a> + 'a {
        self.0.key()
    }

    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    ///
    /// This method is a shortcut for calling `key` and concatenating the returned slices.
    pub fn key_as_vec(&self) -> Vec<u8> {
        self.0.key_as_vec()
    }

    /// Injects the corresponding storage value.
    pub fn inject_value(self, value: Option<impl Iterator<Item = impl AsRef<[u8]>>>) -> Query {
        Query::from_inner(self.0.inject_value(value))
    }
}

/// Fetching the key that follows a given one is required in order to continue.
#[must_use]
pub struct NextKey(read_only_runtime_host::NextKey);

impl NextKey {
    /// Returns the key whose next key must be passed back.
    pub fn key(&self) -> &[u8] {
        self.0.key()
    }

    /// Injects the key.
    ///
    /// # Panic
    ///
    /// Panics if the key passed as parameter isn't strictly superior to the requested key.
    ///
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> Query {
        Query::from_inner(self.0.inject_key(key))
    }
}

#[derive(Decode, Encode)]
struct DecodableBabeEpochInformation {
    epoch_index: u64,
    start_slot_number: Option<u64>,
    authorities: Vec<DecodableBabeAuthority>,
    randomness: [u8; 32],
    c: (u64, u64),
    allowed_slots: header::BabeAllowedSlots,
}

#[derive(Decode, Encode)]
struct DecodableBabeAuthority {
    public_key: [u8; 32],
    weight: u64,
}
