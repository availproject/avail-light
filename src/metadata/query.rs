//! Retrieving the metadata from the runtime.
//!
//! The metadata can be obtained by calling the `Metadata_metadata` entry point of the runtime.
//! The runtime normally straight-up outputs some hardcoded structures, and no access to the
//! storage (or any other external function) is necessary.
//!
//! # About the length prefix
//!
//! The Wasm runtime returns the metadata prefixed with a SCALE-compact-encoded prefix. The
//! functions in this module remove this prefix before returning the value.
//!
//! While it would be more idiomatic to return the raw result and let the user remove the prefix
//! if desired, the presence of this prefix is clearly the result of a mistake during the
//! development process that has now to be kept in order to preserve backwards compatibility.
//!
//! What the documentation refers as "the metadata" systematically describes the metadata
//! *without* a length prefix, and it is therefore less surprising to not include this length
//! prefix in the return value of this function.
//!

use crate::executor;

use core::convert::TryFrom as _;
use parity_scale_codec::Decode as _;

/// Retrieves the SCALE-encoded metadata from the runtime code of a block.
///
/// > **Note**: This function is a convenient shortcut for
/// >           [`metadata_from_virtual_machine_prototype`]. In performance-critical situations,
/// >           where the overhead of the Wasm compilation is undesirable, you are encouraged to
/// >           call [`metadata_from_virtual_machine_prototype`] instead.
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

    let mut outcome = loop {
        match vm.state() {
            executor::State::ReadyToRun(r) => r.run(),
            executor::State::Finished(data) => break data.to_vec(),
            executor::State::Trapped => return Err(Error::Trapped),
            executor::State::LogEmit { resolve, .. } => resolve.finish_call(()),

            // Querying the metadata shouldn't require any extrinsic such as accessing the
            // storage.
            _ => return Err(Error::ExternalityNotAllowed),
        }
    };

    remove_length_prefix(&mut outcome)?;

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
fn remove_length_prefix(metadata: &mut Vec<u8>) -> Result<(), Error> {
    // TODO: maybe don't use parity_scale_codec here
    let length = parity_scale_codec::Compact::<u64>::decode(&mut (&metadata[..]))
        .map_err(|_| Error::BadLengthPrefix)?;
    // This `CompactLen` API is one of the weird APIs I've ever seen.
    let len_len =
        <parity_scale_codec::Compact<u64> as parity_scale_codec::CompactLen<u64>>::compact_len(
            &length.0,
        );
    if usize::try_from(length.0)
        .unwrap_or(usize::max_value())
        .checked_add(len_len)
        .ok_or(Error::BadLengthPrefix)?
        != metadata.len()
    {
        return Err(Error::BadLengthPrefix);
    }
    *metadata = metadata[len_len..].to_owned();
    Ok(())
}
