use crate::{executor, header};

use core::convert::TryFrom as _;
use parity_scale_codec::DecodeAll as _;

/// Grandpa configuration of a chain, as extracted from the genesis block.
///
/// The way a chain configures Grandpa is stored in its runtime.
// TODO: for some chains, also stored in the storage under a well known key
#[derive(Debug, Clone)]
pub struct GrandpaGenesisConfiguration {
    /// Authorities of the authorities set 0. These are the authorities that finalize block #1.
    pub initial_authorities: Vec<header::GrandpaAuthority>,
}

type ConfigScaleEncoding = Vec<([u8; 32], u64)>;

impl GrandpaGenesisConfiguration {
    /// Retrieves the configuration from the given runtime code.
    ///
    /// Must be passed a closure that returns the storage value corresponding to the given key in
    /// the genesis block storage.
    pub fn from_genesis_storage(
        mut genesis_storage_access: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    ) -> Result<Self, FromGenesisStorageError> {
        let wasm_code =
            genesis_storage_access(b":code").ok_or(FromGenesisStorageError::RuntimeNotFound)?;
        let vm = executor::WasmVmPrototype::new(&wasm_code)
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
        // TODO: DRY with the babe config; put a helper in the executor module
        let mut vm = vm
            .run_no_param("GrandpaApi_grandpa_authorities")
            .map_err(FromVmPrototypeError::VmInitialization)?;

        let inner = loop {
            match vm.state() {
                executor::State::ReadyToRun(r) => r.run(),
                executor::State::Finished(data) => {
                    break match ConfigScaleEncoding::decode_all(&data) {
                        Ok(cfg) => cfg,
                        Err(err) => return Err(FromVmPrototypeError::OutputDecode(err)),
                    };
                }
                executor::State::Trapped => return Err(FromVmPrototypeError::Trapped),

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

                _ => return Err(FromVmPrototypeError::ExternalityNotAllowed),
            }
        };

        let initial_authorities = inner
            .into_iter()
            .map(|(public_key, weight)| header::GrandpaAuthority { public_key, weight })
            .collect();

        let outcome = GrandpaGenesisConfiguration {
            initial_authorities,
        };

        Ok((outcome, vm.into_prototype()))
    }
}

/// Error when retrieving the Grandpa configuration.
#[derive(Debug, derive_more::Display)]
pub enum FromGenesisStorageError {
    /// Runtime couldn't be found in the genesis storage.
    RuntimeNotFound,
    /// Error while executing the runtime.
    VmError(FromVmPrototypeError),
}

/// Error when retrieving the Grandpa configuration.
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
