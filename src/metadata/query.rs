//! Retrieving the metadata from the runtime.
//!
//! The metadata can be obtained by calling the `Metadata_metadata` entry point of the runtime.
//! The runtime normally straight-up outputs some hardcoded structures, and no access to the
//! storage (or any other external function) is necessary.
//!
//! # About the length prefix
//!
//! The Wasm runtime returns the metadata prefixed with a SCALE-compact-encoded length. The
//! functions in this module remove this length prefix before returning the value.
//!
//! While it would be more flexible to return the raw result and let the user remove the prefix
//! if desired, the presence of this prefix is clearly the result of a mistake during the
//! development process that now has to be maintained in order to preserve backwards
//! compatibility.
//!
//! A lot of documentation concerning the metadata is available on the Internet, and several
//! projects are capable of parsing the metadata, and what is referred to as "the metadata"
//! in this documentation and projects systematically describes the metadata *without* any length
//! prefix. It would be confusing and error-prone if the value returned here obeyed a slightly
//! different definition.
//!

use crate::executor;

/// Retrieves the SCALE-encoded metadata from the runtime code of a block.
///
/// > **Note**: This function is a convenient shortcut for
/// >           [`metadata_from_virtual_machine_prototype`]. In performance-critical situations,
/// >           where the overhead of the Wasm compilation is undesirable, you are encouraged to
/// >           call [`metadata_from_virtual_machine_prototype`] instead.
// TODO: document heap_pages
pub fn metadata_from_runtime_code(wasm_code: &[u8], heap_pages: u64) -> Result<Vec<u8>, Error> {
    let vm =
        executor::WasmVmPrototype::new(&wasm_code, heap_pages).map_err(Error::VmInitialization)?;
    let (out, _vm) = metadata_from_virtual_machine_prototype(vm)?;
    Ok(out)
}

/// Retrieves the SCALE-encoded metadata from the given virtual machine prototype.
///
/// Returns back the same virtual machine prototype as was passed as parameter.
pub fn metadata_from_virtual_machine_prototype(
    vm: executor::WasmVmPrototype,
) -> Result<(Vec<u8>, executor::WasmVmPrototype), Error> {
    let mut vm = vm
        .run_no_param("Metadata_metadata")
        .map_err(Error::VmInitialization)?;

    let outcome = loop {
        match vm.state() {
            executor::State::ReadyToRun(r) => r.run(),
            executor::State::Finished(data) => break remove_length_prefix(data)?.to_owned(),
            executor::State::Trapped => return Err(Error::Trapped),
            executor::State::LogEmit { resolve, .. } => resolve.finish_call(()),

            // Querying the metadata shouldn't require any extrinsic such as accessing the
            // storage.
            _ => return Err(Error::ExternalityNotAllowed),
        }
    };

    Ok((outcome, vm.into_prototype()))
}

/// Error when retrieving the metadata.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Error when initializing the virtual machine.
    VmInitialization(executor::NewErr),
    /// Crash while running the virtual machine.
    Trapped,
    /// Virtual machine tried to call an externality that isn't valid in this context.
    ExternalityNotAllowed,
    /// Length prefix doesn't match actual length of the metadata.
    BadLengthPrefix,
}

/// Removes the length prefix at the beginning of `metadata`. Returns an error if there is no
/// valid length prefix.
fn remove_length_prefix(metadata: &[u8]) -> Result<&[u8], Error> {
    let (after_prefix, length) = crate::util::nom_scale_compact_usize(metadata)
        .map_err(|_: nom::Err<(&[u8], nom::error::ErrorKind)>| Error::BadLengthPrefix)?;

    // Verify that the length prefix indeed matches the metadata's length.
    if length != after_prefix.len() {
        return Err(Error::BadLengthPrefix);
    }

    Ok(after_prefix)
}
