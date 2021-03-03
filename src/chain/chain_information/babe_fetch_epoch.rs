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
    executor::{host, read_only_runtime_host},
    header,
};

use alloc::vec::Vec;
use parity_scale_codec::{Decode, Encode};

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
    WasmStart(host::StartErr, host::HostVmPrototype),
    /// Error while running the Wasm virtual machine.
    #[display(fmt = "{}", _0)]
    WasmVm(read_only_runtime_host::Error),
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
        Err((err, proto)) => Query::Finished(Err(Error::WasmStart(err, proto))),
    }
}

/// Partial information about a Babe epoch.
pub struct PartialBabeEpochInformation {
    pub epoch_index: u64,
    pub start_slot_number: Option<u64>,
    pub authorities: Vec<header::BabeAuthority>,
    pub randomness: [u8; 32],
}

/// Current state of the operation.
#[must_use]
pub enum Query {
    /// Fetching the Babe epoch is over.
    Finished(Result<(PartialBabeEpochInformation, host::HostVmPrototype), Error>),
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
                let decoded = DecodableBabeEpochInformation::decode(
                    &mut success.virtual_machine.value().as_ref(),
                );

                match decoded {
                    Ok(epoch) => Query::Finished(Ok((
                        PartialBabeEpochInformation {
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
            56, 28, 0, 0, 0, 0, 0, 0, 183, 64, 4, 16, 0, 0, 0, 0, 88, 2, 0, 0, 0, 0, 0, 0, 60, 60,
            97, 197, 140, 161, 35, 106, 9, 104, 63, 251, 75, 144, 118, 18, 29, 88, 185, 48, 164,
            143, 72, 93, 222, 75, 226, 200, 243, 57, 201, 118, 21, 1, 0, 0, 0, 0, 0, 0, 0, 72, 105,
            150, 202, 243, 225, 149, 178, 90, 117, 160, 248, 93, 21, 236, 233, 121, 194, 129, 13,
            194, 139, 82, 228, 203, 126, 230, 106, 158, 203, 20, 61, 1, 0, 0, 0, 0, 0, 0, 0, 120,
            247, 35, 155, 29, 63, 210, 75, 46, 34, 22, 61, 148, 66, 136, 114, 59, 233, 15, 92, 58,
            180, 145, 38, 15, 130, 239, 86, 184, 238, 37, 8, 1, 0, 0, 0, 0, 0, 0, 0, 244, 172, 144,
            91, 15, 86, 0, 219, 89, 86, 154, 133, 200, 106, 49, 249, 156, 212, 209, 73, 49, 112,
            157, 175, 95, 74, 111, 148, 136, 6, 150, 74, 1, 0, 0, 0, 0, 0, 0, 0, 36, 252, 143, 20,
            44, 163, 149, 46, 24, 54, 105, 255, 34, 228, 9, 100, 92, 216, 80, 241, 158, 26, 136,
            232, 203, 10, 189, 174, 210, 52, 117, 12, 1, 0, 0, 0, 0, 0, 0, 0, 226, 224, 72, 174,
            181, 236, 236, 251, 33, 91, 57, 64, 68, 29, 57, 163, 3, 179, 240, 46, 143, 31, 221,
            108, 128, 163, 149, 185, 92, 130, 172, 6, 1, 0, 0, 0, 0, 0, 0, 0, 54, 26, 79, 126, 74,
            132, 12, 26, 50, 91, 228, 1, 140, 245, 62, 210, 228, 18, 216, 231, 183, 234, 134, 177,
            12, 41, 50, 214, 88, 243, 93, 51, 1, 0, 0, 0, 0, 0, 0, 0, 178, 55, 231, 234, 22, 84,
            107, 248, 251, 136, 165, 252, 218, 141, 73, 245, 149, 40, 114, 234, 243, 255, 249, 244,
            48, 36, 75, 31, 28, 195, 136, 41, 1, 0, 0, 0, 0, 0, 0, 0, 224, 77, 167, 191, 55, 175,
            174, 214, 210, 95, 118, 134, 197, 147, 96, 214, 90, 64, 233, 11, 207, 112, 72, 138,
            230, 5, 131, 17, 114, 241, 240, 5, 1, 0, 0, 0, 0, 0, 0, 0, 174, 228, 151, 65, 101, 59,
            215, 82, 126, 147, 118, 12, 49, 248, 100, 20, 126, 41, 16, 73, 162, 13, 223, 253, 34,
            206, 207, 180, 97, 133, 93, 12, 1, 0, 0, 0, 0, 0, 0, 0, 198, 2, 2, 108, 95, 64, 139,
            172, 245, 38, 202, 193, 153, 18, 232, 69, 112, 117, 104, 105, 190, 163, 81, 213, 184,
            41, 188, 124, 206, 159, 158, 101, 1, 0, 0, 0, 0, 0, 0, 0, 134, 62, 201, 97, 144, 120,
            161, 90, 90, 76, 111, 227, 172, 5, 230, 35, 178, 192, 59, 243, 42, 149, 243, 62, 215,
            175, 28, 84, 192, 243, 7, 83, 1, 0, 0, 0, 0, 0, 0, 0, 6, 231, 39, 255, 69, 74, 142, 80,
            58, 243, 174, 47, 127, 251, 19, 72, 239, 124, 172, 199, 80, 33, 232, 211, 51, 226, 180,
            236, 131, 38, 146, 68, 1, 0, 0, 0, 0, 0, 0, 0, 100, 218, 128, 168, 54, 65, 39, 134,
            166, 245, 23, 111, 67, 120, 237, 235, 96, 193, 163, 55, 230, 200, 73, 10, 35, 80, 68,
            226, 135, 252, 65, 68, 1, 0, 0, 0, 0, 0, 0, 0, 4, 137, 113, 107, 23, 103, 214, 113,
            181, 180, 178, 84, 240, 214, 246, 172, 192, 216, 253, 150, 200, 231, 45, 187, 242, 215,
            236, 224, 104, 36, 94, 86, 1, 0, 0, 0, 0, 0, 0, 0, 158, 0, 242, 159, 152, 81, 251, 247,
            62, 14, 42, 94, 43, 144, 26, 193, 253, 127, 175, 217, 134, 74, 124, 230, 87, 141, 242,
            114, 212, 93, 3, 108,
        ];

        super::DecodableBabeEpochInformation::decode_all(&sample_data).unwrap();
    }
}
