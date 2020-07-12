use super::definitions;
use crate::executor;

use core::convert::TryFrom as _;
use parity_scale_codec::DecodeAll as _;

/// BABE configuration of a chain, as extracted from the genesis block.
///
/// The way a chain configures BABE is stored in its runtime.
pub struct BabeGenesisConfiguration {
    inner: definitions::BabeGenesisConfiguration,
}

impl BabeGenesisConfiguration {
    /// Retrieves the configuration from the given runtime code.
    ///
    /// Must be passed a closure that returns the storage value corresponding to the given key in
    /// the genesis block storage.
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

        let outcome = loop {
            match vm.state() {
                executor::State::ReadyToRun(r) => r.run(),
                executor::State::Finished(data) => {
                    break match definitions::BabeGenesisConfiguration::decode_all(&data) {
                        Ok(cfg) => BabeGenesisConfiguration { inner: cfg },
                        Err(err) => return Err(BabeChainConfigurationError::OutputDecode(err)),
                    };
                }
                executor::State::Finished(_) => unreachable!(),
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

        Ok((outcome, vm.into_prototype()))
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
