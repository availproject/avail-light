use crate::executor;
use core::convert::TryFrom as _;

// TODO: check whether we actually need storage access
// TODO: if we need storage access, it should also have the next_key and prefix_keys functions implemented

/// Retrieves the SCALE-encoded metadata from the storage of a block.
///
/// Must be passed a closure that returns the storage value corresponding to the given key in
/// the block storage.
pub fn from_storage(
    mut storage_access: impl FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, FromStorageError> {
    let wasm_code = storage_access(b":code").ok_or(FromStorageError::RuntimeNotFound)?;
    let vm = executor::WasmVmPrototype::new(&wasm_code)
        .map_err(FromVmPrototypeError::VmInitialization)
        .map_err(FromStorageError::VmError)?;
    let (cfg, _) =
        from_virtual_machine_prototype(vm, storage_access).map_err(FromStorageError::VmError)?;
    Ok(cfg)
}

/// Retrieves the SCALE-encoded metadata from the given virtual machine prototype.
///
/// Must be passed a closure that returns the storage value corresponding to the given key in
/// the block storage.
///
/// Returns back the same virtual machine prototype as was passed as parameter.
pub fn from_virtual_machine_prototype(
    vm: executor::WasmVmPrototype,
    mut storage_access: impl FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Result<(Vec<u8>, executor::WasmVmPrototype), FromVmPrototypeError> {
    let mut vm = vm
        .run_no_param("Metadata_metadata")
        .map_err(FromVmPrototypeError::VmInitialization)?;

    let outcome = loop {
        match vm.state() {
            executor::State::ReadyToRun(r) => r.run(),
            executor::State::Finished(data) => break data.to_vec(),
            executor::State::Trapped => return Err(FromVmPrototypeError::Trapped),

            executor::State::ExternalStorageGet {
                storage_key,
                offset,
                max_size,
                resolve,
            } => {
                let mut value = storage_access(storage_key);

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

            executor::State::LogEmit { resolve, .. } => resolve.finish_call(()),

            _ => return Err(FromVmPrototypeError::ExternalityNotAllowed),
        }
    };

    Ok((outcome, vm.into_prototype()))
}

/// Error when retrieving the BABE configuration.
#[derive(Debug, derive_more::Display)]
pub enum FromStorageError {
    /// Runtime couldn't be found in the genesis storage.
    RuntimeNotFound,
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
