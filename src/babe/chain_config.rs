use super::header_info;
use crate::{executor, header};

use core::convert::TryFrom as _;
use parity_scale_codec::DecodeAll as _;

/// BABE configuration of a chain, as extracted from the genesis block.
///
/// The way a chain configures BABE is stored in its runtime.
pub struct BabeGenesisConfiguration {
    inner: OwnedGenesisConfiguration,
    epoch0_information: header_info::EpochInformation,
}

impl BabeGenesisConfiguration {
    /// Retrieves the configuration from the given runtime code.
    ///
    /// Must be passed a closure that returns the storage value corresponding to the given key in
    /// the genesis block storage.
    // TODO: why is `wasm_code` a separate parameter? it could grabbed with `genesis_storage_access`
    pub fn from_runtime_code(
        wasm_code: impl AsRef<[u8]>,
        genesis_storage_access: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> Result<Self, BabeChainConfigurationError> {
        let vm = executor::WasmVmPrototype::new(wasm_code)
            .map_err(BabeChainConfigurationError::VmInitialization)?;
        let (cfg, _) = Self::from_virtual_machine_prototype(vm, genesis_storage_access)?;
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
    ) -> Result<(Self, executor::WasmVmPrototype), BabeChainConfigurationError> {
        let mut vm = vm
            .run_no_param("BabeApi_configuration")
            .map_err(BabeChainConfigurationError::VmInitialization)?;

        let inner = loop {
            match vm.state() {
                executor::State::ReadyToRun(r) => r.run(),
                executor::State::Finished(data) => {
                    break match OwnedGenesisConfiguration::decode_all(&data) {
                        Ok(cfg) => cfg,
                        Err(err) => return Err(BabeChainConfigurationError::OutputDecode(err)),
                    };
                }
                executor::State::Trapped => return Err(BabeChainConfigurationError::Trapped),

                executor::State::ExternalStorageGet {
                    storage_key,
                    offset,
                    max_size,
                    resolve,
                } => {
                    let mut value = genesis_storage_access(storage_key);

                    // TODO: maybe this could be a utility function in `executor`
                    if let Some(value) = &mut value {
                        if usize::try_from(offset).unwrap() < value.len() {
                            *value = value[usize::try_from(offset).unwrap()..].to_vec();
                            if usize::try_from(max_size).unwrap() < value.len() {
                                *value = value[..usize::try_from(max_size).unwrap()].to_vec();
                            }
                        } else {
                            *value = Vec::new();
                        }
                    }

                    resolve.finish_call(value);
                }

                _ => return Err(BabeChainConfigurationError::ExternalityNotAllowed),
            }
        };

        let epoch0_information = header_info::EpochInformation {
            randomness: inner.randomness,
            authorities: inner
                .genesis_authorities
                .iter()
                .map(
                    |(public_key, weight)| header_info::EpochInformationAuthority {
                        public_key: *public_key,
                        weight: *weight,
                    },
                )
                .collect(),
        };

        let outcome = BabeGenesisConfiguration {
            inner,
            epoch0_information,
        };

        Ok((outcome, vm.into_prototype()))
    }

    // TODO: docs
    // TODO: should be part of a `BabeConfiguration` struct instead
    pub fn c(&self) -> (u64, u64) {
        self.inner.c
    }

    // TODO: docs
    // TODO: should be part of a `BabeConfiguration` struct instead
    pub fn slot_duration(&self) -> u64 {
        self.inner.slot_duration
    }

    // TODO: docs
    // TODO: should be part of a `BabeConfiguration` struct instead
    pub fn slots_per_epoch(&self) -> u64 {
        self.inner.epoch_length
    }

    /// Returns the information about epoch number 0, which starts at block number 1. Block number
    /// 1 contains the information about epoch number 1.
    pub fn epoch0_information(&self) -> &header_info::EpochInformation {
        &self.epoch0_information
    }
}

/// Error when retrieving the BABE configuration.
#[derive(Debug, derive_more::Display)]
pub enum BabeChainConfigurationError {
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
