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

use crate::{executor, header};

use core::{convert::TryFrom as _, fmt};
use parity_scale_codec::DecodeAll as _;

/// BABE configuration of a chain, as extracted from the genesis block.
///
/// The way a chain configures BABE is stored in its runtime.
#[derive(Clone)]
pub struct BabeGenesisConfiguration {
    inner: OwnedGenesisConfiguration,
    epoch0_information: header::BabeNextEpoch,
}

impl BabeGenesisConfiguration {
    /// Retrieves the configuration from the storage of the genesis block.
    ///
    /// Must be passed a closure that returns the storage value corresponding to the given key in
    /// the genesis block storage.
    pub fn from_genesis_storage(
        mut genesis_storage_access: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> Result<Self, FromGenesisStorageError> {
        let wasm_code =
            genesis_storage_access(b":code").ok_or(FromGenesisStorageError::RuntimeNotFound)?;
        let heap_pages = if let Some(bytes) = genesis_storage_access(b":heappages") {
            u64::from_le_bytes(
                <[u8; 8]>::try_from(&bytes[..])
                    .map_err(FromGenesisStorageError::HeapPagesDecode)?,
            )
        } else {
            1024 // TODO: default heap pages
        };
        let vm = executor::WasmVmPrototype::new(&wasm_code, heap_pages)
            .map_err(FromVmPrototypeError::VmInitialization)
            .map_err(FromGenesisStorageError::VmError)?;
        let (cfg, _) = Self::from_virtual_machine_prototype(vm, genesis_storage_access)
            .map_err(FromGenesisStorageError::VmError)?;
        Ok(cfg)
    }

    /// Retrieves the configuration from the given virtual machine prototype.
    ///
    /// Must be passed a closure that returns the storage value corresponding to the given key in
    /// the genesis block storage.
    ///
    /// Returns back the same virtual machine prototype as was passed as parameter.
    pub fn from_virtual_machine_prototype(
        vm: executor::WasmVmPrototype,
        mut genesis_storage_access: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> Result<(Self, executor::WasmVmPrototype), FromVmPrototypeError> {
        let mut vm: executor::WasmVm = vm
            .run_no_param("BabeApi_configuration")
            .map_err(FromVmPrototypeError::VmInitialization)?
            .into();

        let (inner, vm_prototype) = loop {
            match vm {
                executor::WasmVm::ReadyToRun(r) => vm = r.run(),
                executor::WasmVm::Finished(finished) => {
                    break match OwnedGenesisConfiguration::decode_all(finished.value()) {
                        Ok(cfg) => (cfg, finished.into_prototype()),
                        Err(err) => return Err(FromVmPrototypeError::OutputDecode(err)),
                    };
                }
                executor::WasmVm::Trapped { .. } => return Err(FromVmPrototypeError::Trapped),

                executor::WasmVm::ExternalStorageGet(req) => {
                    let value = genesis_storage_access(req.key());
                    vm = req.resume_full_value(value.as_ref().map(|v| &v[..]));
                }

                executor::WasmVm::LogEmit(req) => vm = req.resume(),

                _ => return Err(FromVmPrototypeError::ExternalityNotAllowed),
            }
        };

        let epoch0_information = header::BabeNextEpoch {
            randomness: inner.randomness,
            authorities: inner
                .genesis_authorities
                .iter()
                .map(|(public_key, weight)| header::BabeAuthority {
                    public_key: *public_key,
                    weight: *weight,
                })
                .collect(),
        };

        let outcome = BabeGenesisConfiguration {
            inner,
            epoch0_information,
        };

        Ok((outcome, vm_prototype))
    }

    /// Returns the number of slots contained in each epoch.
    pub fn slots_per_epoch(&self) -> u64 {
        self.inner.epoch_length
    }

    /// Returns the configuration of epoch number 0.
    pub fn epoch0_configuration(&self) -> header::BabeNextConfig {
        header::BabeNextConfig {
            c: self.inner.c,
            allowed_slots: self.inner.allowed_slots,
        }
    }

    /// Returns the information about epoch number 0.
    pub fn epoch0_information(&self) -> header::BabeNextEpochRef {
        From::from(&self.epoch0_information)
    }
}

impl fmt::Debug for BabeGenesisConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: better
        f.debug_struct("BabeGenesisConfiguration").finish()
    }
}

/// Error when retrieving the BABE configuration.
#[derive(Debug, derive_more::Display)]
pub enum FromGenesisStorageError {
    /// Runtime couldn't be found in the genesis storage.
    RuntimeNotFound,
    /// Number of heap pages couldn't be found in the genesis storage.
    HeapPagesNotFound,
    /// Failed to decode heap pages from the genesis storage.
    HeapPagesDecode(core::array::TryFromSliceError),
    /// Error while executing the runtime.
    VmError(FromVmPrototypeError),
}

/// Error when retrieving the BABE configuration.
#[derive(Debug, derive_more::Display)]
pub enum FromVmPrototypeError {
    /// Error when initializing the virtual machine.
    VmInitialization(executor::NewErr),
    /// Crash while running the virtual machine.
    Trapped,
    /// Virtual machine tried to call an externality that isn't valid in this context.
    ExternalityNotAllowed,
    /// Error while decoding the output of the virtual machine.
    OutputDecode(parity_scale_codec::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, parity_scale_codec::Encode, parity_scale_codec::Decode)]
struct OwnedGenesisConfiguration {
    slot_duration: u64,
    epoch_length: u64,
    c: (u64, u64),
    genesis_authorities: Vec<([u8; 32], u64)>,
    randomness: [u8; 32],
    allowed_slots: header::BabeAllowedSlots,
}
