// Smoldot
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
use parity_scale_codec::{Decode, DecodeAll as _, Encode};

/// The Babe epoch to fetch.
pub enum BabeEpochToFetch {
    /// Fetch the current epoch using `BabeApi_current_epoch`.
    CurrentEpoch,
    /// Fetch the next epoch using `BabeApi_next_epoch`.
    NextEpoch,
}

/// Configuration for [`babe_fetch_epoch`].
pub struct Config {
    /// Runtime used to get the Babe epoch. Must be built using the Wasm code found at the
    /// `:code` key of the block storage.
    pub runtime: host::HostVmPrototype,
    /// The Babe epoch to fetch.
    pub epoch_to_fetch: BabeEpochToFetch,
}

/// Problem encountered during a call to [`babe_fetch_epoch`].
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Error while starting the Wasm virtual machine.
    #[display(fmt = "{}", _0)]
    WasmStart(host::StartErr),
    /// Error while running the Wasm virtual machine.
    #[display(fmt = "{}", _0)]
    WasmVm(read_only_runtime_host::ErrorDetail),
    /// Error while decoding the babe epoch.
    #[display(fmt = "{}", _0)]
    DecodeFailed(parity_scale_codec::Error),
}

/// Fetches a Babe epoch using `BabeApi_current_epoch` or `BabeApi_next_epoch`.
pub fn babe_fetch_epoch(config: Config) -> Query {
    let function_to_call = match config.epoch_to_fetch {
        BabeEpochToFetch::CurrentEpoch => "BabeApi_current_epoch",
        BabeEpochToFetch::NextEpoch => "BabeApi_next_epoch",
    };

    let vm = read_only_runtime_host::run(read_only_runtime_host::Config {
        virtual_machine: config.runtime,
        function_to_call,
        // The epoch functions don't take any parameters.
        parameter: core::iter::empty::<&[u8]>(),
    });

    match vm {
        Ok(vm) => Query::from_inner(vm),
        Err((err, virtual_machine)) => Query::Finished {
            result: Err(Error::WasmStart(err)),
            virtual_machine,
        },
    }
}

/// Current state of the operation.
#[must_use]
pub enum Query {
    /// Fetching the Babe epoch is over.
    Finished {
        result: Result<BabeEpochInformation, Error>,
        virtual_machine: host::HostVmPrototype,
    },
    /// Loading a storage value is required in order to continue.
    StorageGet(StorageGet),
    /// Fetching the key that follows a given one is required in order to continue.
    NextKey(NextKey),
    /// Fetching the storage trie root is required in order to continue.
    StorageRoot(StorageRoot),
}

impl Query {
    fn from_inner(inner: read_only_runtime_host::RuntimeHostVm) -> Self {
        match inner {
            read_only_runtime_host::RuntimeHostVm::Finished(Ok(success)) => {
                let decoded = DecodableBabeEpochInformation::decode_all(
                    &mut success.virtual_machine.value().as_ref(),
                );

                let virtual_machine = success.virtual_machine.into_prototype();

                match decoded {
                    Ok(epoch) => Query::Finished {
                        result: Ok(BabeEpochInformation {
                            epoch_index: epoch.epoch_index,
                            start_slot_number: Some(epoch.start_slot_number),
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
                        }),
                        virtual_machine,
                    },
                    Err(error) => Query::Finished {
                        result: Err(Error::DecodeFailed(error)),
                        virtual_machine,
                    },
                }
            }
            read_only_runtime_host::RuntimeHostVm::Finished(Err(err)) => Query::Finished {
                result: Err(Error::WasmVm(err.detail)),
                virtual_machine: err.prototype,
            },
            read_only_runtime_host::RuntimeHostVm::StorageGet(inner) => {
                Query::StorageGet(StorageGet(inner))
            }
            read_only_runtime_host::RuntimeHostVm::StorageRoot(inner) => {
                Query::StorageRoot(StorageRoot(inner))
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
    pub fn key(&'_ self) -> impl Iterator<Item = impl AsRef<[u8]> + '_> + '_ {
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
    pub fn key(&'_ self) -> impl AsRef<[u8]> + '_ {
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

/// Fetching the storage trie root is required in order to continue.
#[must_use]
pub struct StorageRoot(read_only_runtime_host::StorageRoot);

impl StorageRoot {
    /// Writes the trie root hash to the Wasm VM and prepares it for resume.
    pub fn resume(self, hash: &[u8; 32]) -> Query {
        Query::from_inner(self.0.resume(hash))
    }
}

#[derive(Decode, Encode)]
struct DecodableBabeEpochInformation {
    epoch_index: u64,
    start_slot_number: u64,
    duration: u64,
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

#[cfg(test)]
mod tests {
    use parity_scale_codec::DecodeAll;

    #[test]
    fn sample_decode() {
        // Sample taken from an actual Westend block.
        let sample_data = [
            100, 37, 0, 0, 0, 0, 0, 0, 215, 191, 25, 16, 0, 0, 0, 0, 88, 2, 0, 0, 0, 0, 0, 0, 16,
            102, 85, 132, 42, 246, 238, 38, 228, 88, 181, 254, 162, 211, 181, 190, 178, 221, 140,
            249, 107, 36, 180, 72, 56, 145, 158, 26, 226, 150, 72, 223, 12, 1, 0, 0, 0, 0, 0, 0, 0,
            92, 167, 131, 48, 94, 202, 168, 131, 131, 232, 44, 215, 20, 97, 44, 22, 227, 205, 24,
            232, 243, 118, 34, 15, 45, 159, 187, 181, 132, 214, 138, 105, 1, 0, 0, 0, 0, 0, 0, 0,
            212, 81, 34, 24, 150, 248, 208, 236, 69, 62, 90, 78, 252, 0, 125, 32, 86, 208, 73, 44,
            151, 210, 88, 169, 187, 105, 170, 28, 165, 137, 126, 3, 1, 0, 0, 0, 0, 0, 0, 0, 236,
            198, 169, 213, 112, 57, 219, 36, 157, 140, 107, 231, 182, 155, 98, 72, 224, 156, 194,
            252, 107, 138, 97, 201, 177, 9, 13, 248, 167, 93, 218, 91, 1, 0, 0, 0, 0, 0, 0, 0, 150,
            40, 172, 215, 156, 152, 22, 33, 79, 35, 203, 8, 40, 43, 0, 242, 126, 30, 241, 56, 206,
            56, 36, 189, 60, 22, 121, 195, 168, 34, 207, 236, 1, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0,
            0, 0, 0, 0, 2,
        ];

        super::DecodableBabeEpochInformation::decode_all(&sample_data).unwrap();
    }
}
