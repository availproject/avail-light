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

use crate::{executor, header};

use alloc::vec::Vec;
use core::{convert::TryFrom as _, num::NonZeroU64};

/// Aura configuration of a chain, as extracted from the genesis block.
///
/// The way a chain configures Aura is stored in its runtime.
#[derive(Debug, Clone)]
pub struct AuraGenesisConfiguration {
    /// List of authorities that can validate block #1.
    pub authorities_list: Vec<header::AuraAuthority>,

    /// Duration, in milliseconds, of a slot.
    pub slot_duration: NonZeroU64,
}

impl AuraGenesisConfiguration {
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
        let vm =
            executor::WasmVmPrototype::new(&wasm_code, heap_pages, executor::vm::ExecHint::Oneshot)
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
            .run_no_param("AuraApi_slot_duration")
            .map_err(FromVmPrototypeError::VmInitialization)?
            .into();

        let (slot_duration, vm_prototype) = loop {
            match vm {
                executor::WasmVm::ReadyToRun(r) => vm = r.run(),
                executor::WasmVm::Finished(finished) => {
                    let slot_duration = NonZeroU64::new(u64::from_le_bytes(
                        <[u8; 8]>::try_from(finished.value())
                            .map_err(|_| FromVmPrototypeError::BadSlotDuration)?,
                    ))
                    .ok_or(FromVmPrototypeError::BadSlotDuration)?;
                    break (slot_duration, finished.into_prototype());
                }
                executor::WasmVm::Error { .. } => return Err(FromVmPrototypeError::Trapped),

                executor::WasmVm::ExternalStorageGet(req) => {
                    let value = genesis_storage_access(req.key());
                    vm = req.resume_full_value(value.as_ref().map(|v| &v[..]));
                }

                executor::WasmVm::LogEmit(req) => vm = req.resume(),

                _ => return Err(FromVmPrototypeError::ExternalityNotAllowed),
            }
        };

        let mut vm: executor::WasmVm = vm_prototype
            .run_no_param("AuraApi_authorities")
            .map_err(FromVmPrototypeError::VmInitialization)?
            .into();

        let (authorities_list, vm_prototype) = loop {
            match vm {
                executor::WasmVm::ReadyToRun(r) => vm = r.run(),
                executor::WasmVm::Finished(finished) => {
                    let authorities_list = header::AuraAuthoritiesIter::decode(finished.value())
                        .map_err(|_| FromVmPrototypeError::AuthoritiesListDecodeError)?
                        .map(header::AuraAuthority::from)
                        .collect::<Vec<_>>();
                    break (authorities_list, finished.into_prototype());
                }
                executor::WasmVm::Error { .. } => return Err(FromVmPrototypeError::Trapped),

                executor::WasmVm::ExternalStorageGet(req) => {
                    let value = genesis_storage_access(req.key());
                    vm = req.resume_full_value(value.as_ref().map(|v| &v[..]));
                }

                executor::WasmVm::LogEmit(req) => vm = req.resume(),

                _ => return Err(FromVmPrototypeError::ExternalityNotAllowed),
            }
        };

        let outcome = AuraGenesisConfiguration {
            authorities_list,
            slot_duration,
        };

        Ok((outcome, vm_prototype))
    }
}

/// Error when retrieving the Aura configuration.
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

/// Error when retrieving the Aura configuration.
#[derive(Debug, derive_more::Display)]
pub enum FromVmPrototypeError {
    /// Error when initializing the virtual machine.
    VmInitialization(executor::NewErr),
    /// Crash while running the virtual machine.
    Trapped,
    /// Virtual machine tried to call an externality that isn't valid in this context.
    ExternalityNotAllowed,
    /// Error while decoding the output of the virtual machine for `AuraApi_slot_duration`.
    BadSlotDuration,
    /// Failed to decode the list of authorities returned by `AuraApi_authorities`.
    AuthoritiesListDecodeError,
}
