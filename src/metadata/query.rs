use crate::executor;

/// Retrieves the SCALE-encoded metadata from the runtime code of a block.
pub fn metadata_from_runtime_code(wasm_code: &[u8]) -> Result<Vec<u8>, FromStorageError> {
    let vm = executor::WasmVmPrototype::new(&wasm_code)
        .map_err(FromVmPrototypeError::VmInitialization)
        .map_err(FromStorageError::VmError)?;
    let (out, _) = metadata_from_virtual_machine_prototype(vm).map_err(FromStorageError::VmError)?;
    Ok(out)
}

/// Retrieves the SCALE-encoded metadata from the given virtual machine prototype.
///
/// Returns back the same virtual machine prototype as was passed as parameter.
pub fn metadata_from_virtual_machine_prototype(
    vm: executor::WasmVmPrototype,
) -> Result<(Vec<u8>, executor::WasmVmPrototype), FromVmPrototypeError> {
    let mut vm = vm
        .run_no_param("Metadata_metadata")
        .map_err(FromVmPrototypeError::VmInitialization)?;

    let outcome = loop {
        match vm.state() {
            executor::State::ReadyToRun(r) => r.run(),
            executor::State::Finished(data) => break data.to_vec(),
            executor::State::Trapped => return Err(FromVmPrototypeError::Trapped),
            executor::State::LogEmit { resolve, .. } => resolve.finish_call(()),
            _ => return Err(FromVmPrototypeError::ExternalityNotAllowed),
        }
    };

    Ok((outcome, vm.into_prototype()))
}

/// Error when retrieving the metadata.
#[derive(Debug, derive_more::Display)]
pub enum FromStorageError {
    /// Error while executing the runtime.
    VmError(FromVmPrototypeError),
}

/// Error when retrieving the metadata.
#[derive(Debug, derive_more::Display)]
pub enum FromVmPrototypeError {
    /// Error when initializing the virtual machine.
    VmInitialization(executor::NewErr),
    /// Crash while running the virtual machine.
    Trapped,
    /// Virtual machine tried to call an externality that isn't valid in this context.
    ExternalityNotAllowed,
}
