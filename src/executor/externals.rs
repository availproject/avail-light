//! Wasm virtual machine specific to the Substrate/Polkadot Runtime Environment.
//!
//! Contrary to [`VirtualMachine`](super::vm::VirtualMachine), this code is not just a generic
//! Wasm virtual machine, but is aware of the Substrate/Polkadot runtime environment. The external
//! functions that the Wasm code calls are automatically resolved and either handled or notified
//! to the user of this module.
//!
//! Any external function that requires pure CPU computations (for example building or verifying
//! a cryptographic signature) is directly handled by the code in this module. Other external
//! functions (for example accessing the state or printing a message) are instead handled by
//! interrupting the virtual machine and waiting for the user of this module to handle the call.
//!
//! > **Note**: The `ext_offchain_random_seed_version_1` and `ext_offchain_timestamp_version_1`
//! >           functions, which requires the host to respectively produce a random seed and
//! >           return the current time, must also be handled by the user. While these functions
//! >           could theoretically be handled directly by this module, it might be useful for
//! >           testing purposes to have the possibility to return a deterministic value.

use super::{allocator, vm};
use crate::block::Block;

use core::{convert::TryFrom as _, hash::Hasher as _, mem};
use parity_scale_codec::{DecodeAll as _, Encode as _};

pub use entry_points::{CoreVersionSuccess, FunctionToCall, Success};

mod entry_points;
mod externalities;

/// A running virtual machine.
pub struct ExternalsVm {
    /// Inner lower-level virtual machine.
    vm: vm::VirtualMachine,

    /// Function currently being called.
    called_function: entry_points::CalledFunction,

    /// State of the virtual machine. Must be in sync with [`ExternalsVm::vm`].
    state: StateInner,

    /// List of functions that the Wasm code imports.
    ///
    /// The keys of this `Vec` (i.e. the `usize` indices) have been passed to the virtual machine
    /// executor. Whenever the Wasm code invokes an external function, we obtain its index, and
    /// look within this `Vec` to know what to do.
    registered_functions: Vec<externalities::Externality>,

    /// Memory allocator in order to answer the calls to `malloc` and `free`.
    allocator: allocator::FreeingBumpHeapAllocator,
}

/// State of the virtual machine.
enum StateInner {
    /// Wasm virtual machine is ready to be run. Will pass the first parameter as the "resume"
    /// value. This is either `None` at initialization, or, if the virtual machine was calling
    /// an externality, the value returned by the externality.
    Ready(Option<vm::RuntimeValue>),
    /// Currently calling an externality. The Wasm virtual machine is paused while we perform all
    /// the outside-of-the-VM operations.
    Calling(externalities::CallState),
    /// The Wasm blob did something that doesn't conform to the runtime environment. This state
    /// never changes to anything else.
    NonConforming(NonConformingErr),
    /// The Wasm VM has encountered a trap (i.e. it has panicked). This state never changes to
    /// anything else.
    Trapped,
    /// Function call has successfully finished. This state never changes to anything else.
    Finished(Success),
    /// Temporary state to permit state transitions without running into borrowing issues.
    Poisoned,
}

impl ExternalsVm {
    /// Creates a new state machine from the given module that executes the given function.
    pub fn new(module: &vm::WasmBlob, to_call: FunctionToCall) -> Result<Self, NewErr> {
        let (called_function, data) = to_call.into_function_and_param();
        let data_len_u32 = u32::try_from(data.len()).map_err(|_| NewErr::DataSizeOverflow)?;

        // Initialize the virtual machine.
        // Each symbol requested by the Wasm runtime will be put in `registered_functions`. Later,
        // when a function is invoked, the Wasm virtual machine will pass indices within that
        // array.
        let (mut vm, registered_functions) = {
            let data_len_i32 = i32::from_ne_bytes(data_len_u32.to_ne_bytes());

            let mut registered_functions = Vec::new();
            let vm = vm::VirtualMachine::new(
                module,
                called_function.symbol_name(),
                &[
                    vm::RuntimeValue::I32(0), // TODO: FIXME: no no no no no /!\
                    vm::RuntimeValue::I32(data_len_i32),
                ],
                // This closure is called back for each function that the runtime imports.
                |mod_name, f_name, _signature| {
                    if mod_name != "env" {
                        return Err(());
                    }

                    let id = registered_functions.len();
                    registered_functions.push(match externalities::function_by_name(f_name) {
                        Some(f) => f,
                        None => return Err(()),
                    });
                    Ok(id)
                },
            )?;
            registered_functions.shrink_to_fit();
            (vm, registered_functions)
        };

        // Initialize the state of the memory allocator. This is the allocator that is later used
        // when the Wasm code requests variable-length data.
        let mut allocator = {
            // In the runtime environment, Wasm blobs must export a global symbol named
            // `__heap_base` indicating where the memory allocator is allowed to allocate memory.
            // TODO: this isn't mentioned in the specs but seems mandatory; report to the specs writers
            let heap_base = vm
                .global_value("__heap_base")
                .map_err(|_| NewErr::HeapBaseNotFound)?;
            allocator::FreeingBumpHeapAllocator::new(heap_base)
        };

        // Use that allocator to write the input data.
        // This has the consequence that the user is allowed to free that input data. If this
        // consequence is unwanted, this code can be changed to write the input data at `heap_base`
        // and start the actual heap after the input data.
        let input_data_location = allocator
            .allocate(&mut MemAccess(&mut vm), data_len_u32)
            .map_err(|_| NewErr::DataSizeOverflow)?;
        vm.write_memory(input_data_location, &data).unwrap();

        Ok(ExternalsVm {
            vm,
            called_function,
            state: StateInner::Ready(None),
            registered_functions,
            allocator,
        })
    }

    /// Returns the current state of the virtual machine.
    pub fn state(&mut self) -> State {
        // Note: the internal structure of this function is unfortunately very weird because we
        // need to bypass limitations in the borrow checker.

        loop {
            if matches!(self.state, StateInner::Calling(_)) {
                self.state = match mem::replace(&mut self.state, StateInner::Poisoned) {
                    // The virtual machine is currently paused and calling an externality. We maintain
                    // a `CallState` object in parallel of the call and run it.
                    StateInner::Calling(mut calling) => match calling.run() {
                        externalities::State::AllocationNeeded { size, inject_value } => {
                            if let Ok(ptr) =
                                self.allocator.allocate(&mut MemAccess(&mut self.vm), size)
                            {
                                inject_value.inject(ptr);
                                StateInner::Calling(calling)
                            } else {
                                StateInner::Trapped
                            }
                        }
                        externalities::State::UntrustedDealloc { pointer, done } => {
                            if self
                                .allocator
                                .deallocate(&mut MemAccess(&mut self.vm), pointer)
                                .is_ok()
                            {
                                done.inject(());
                                StateInner::Calling(calling)
                            } else {
                                StateInner::Trapped
                            }
                        }
                        externalities::State::MemoryReadNeeded {
                            offset,
                            size,
                            inject_value,
                        } => {
                            if let Ok(data) = self.vm.read_memory(offset, size) {
                                inject_value.inject(data);
                                StateInner::Calling(calling)
                            } else {
                                StateInner::Trapped
                            }
                        }
                        externalities::State::MemoryWriteNeeded { offset, data, done } => {
                            if self.vm.write_memory(offset, data).is_ok() {
                                done.inject(());
                                StateInner::Calling(calling)
                            } else {
                                StateInner::Trapped
                            }
                        }
                        _ => unimplemented!(), // TODO:
                    },

                    _ => unreachable!(),
                };
            } else {
                break;
            }
        }

        // We put this one separately because of borrowing issues.
        if let StateInner::Ready(_) = self.state {
            return State::ReadyToRun(ReadyToRun { inner: self });
        }

        match &self.state {
            StateInner::NonConforming(err) => State::NonConforming(err.clone()),
            StateInner::Trapped => State::Trapped,
            StateInner::Finished(success) => State::Finished(success),
            StateInner::Ready(_) | StateInner::Calling(_) | StateInner::Poisoned => unreachable!(),
        }
    }
}

/// State of a [`ExternalVm`]. Mutably borrows the virtual machine, thereby ensuring that the
/// state can't change.
pub enum State<'a> {
    /// Wasm virtual machine is ready to be run. Call [`ReadyToRun::run`] to make progress.
    ReadyToRun(ReadyToRun<'a>),
    /// Function execution has succeeded. Contains the return value of the call.
    Finished(&'a Success),
    /// The Wasm blob did something that doesn't conform to the runtime environment.
    NonConforming(NonConformingErr),
    /// The Wasm VM has encountered a trap (i.e. it has panicked).
    Trapped,
    ExternalStorageGet {
        /// Which key is requested.
        storage_key: Vec<u8>,
        /// Object to use to inject the storage value back. Pass back `None` if key is missing
        /// from storage.
        resolve: Resume<'a, Option<Vec<u8>>>,
    },
    ExternalStorageSet {
        /// Which key to change.
        storage_key: Vec<u8>,
        /// Which storage value to change.
        new_storage_value: Vec<u8>,
        /// Object to use to finish the call
        resolve: Resume<'a, ()>,
    },
    ExternalStorageClear {
        /// Which key to clear.
        storage_key: Vec<u8>,
        /// Object to use to finish the call
        resolve: Resume<'a, ()>,
    },
}

impl<'a> From<ReadyToRun<'a>> for State<'a> {
    fn from(state: ReadyToRun<'a>) -> State<'a> {
        State::ReadyToRun(state)
    }
}

/// Error that can happen when initializing a VM.
#[derive(Debug, derive_more::From, derive_more::Display)]
pub enum NewErr {
    /// Error while initializing the virtual machine.
    #[display(fmt = "Error while initializing the virtual machine: {}", _0)]
    VirtualMachine(vm::NewErr),
    /// The size of the input data is too large.
    DataSizeOverflow,
    /// Couldn't find the `__heap_base` symbol in the Wasm code.
    HeapBaseNotFound,
}

/// Reason why the Wasm blob isn't conforming to the runtime environment.
#[derive(Debug, Clone, derive_more::Display)]
pub enum NonConformingErr {
    /// A non-`i64` value has been returned.
    #[display(fmt = "A non-I64 value has been returned")]
    BadReturnValue, // TODO: indicate what got returned?
    /// The pointer and size returned by the function are invalid.
    #[display(fmt = "The pointer and size returned by the function are invalid")]
    ReturnedPtrOutOfRange {
        /// Pointer that got returned.
        pointer: u32,
        /// Size that got returned.
        size: u32,
        /// Size of the virtual memory.
        memory_size: u32,
    },
    /// Failed to decode the structure returned by the function.
    #[display(fmt = "Failed to decode the structure returned by the function")]
    ReturnedValueDecodeFail(parity_scale_codec::Error),
    /// Failed to decode the value returned by the function.
    SuccessDecode(entry_points::SuccessDecodeErr),
    /// An externality wants to returns a certain value, but the Wasm code expects a different one.
    ExternalityBadReturnValue,
}

/// Virtual machine is ready to run. This mutably borrows the [`ExternalsVm`] and allows making
/// progress.
pub struct ReadyToRun<'a> {
    inner: &'a mut ExternalsVm,
}

impl<'a> ReadyToRun<'a> {
    /// Runs the virtual machine until something important happens.
    ///
    /// > **Note**: This is when the actual CPU-heavy computation happens.
    pub fn run(self) {
        loop {
            // This object can only exist is the state is "ready". We extract the value inside to
            // pass it to the inner state machine.
            let resume_value = match &mut self.inner.state {
                StateInner::Ready(val) => val.take(),
                _ => unreachable!(),
            };

            match self.inner.vm.run(resume_value) {
                Ok(vm::ExecOutcome::Finished {
                    return_value: Ok(Some(vm::RuntimeValue::I64(ret))),
                }) => {
                    // Wasm virtual machine has successfully returned.

                    // TODO: rewrite this code to be cleaner.
                    // Turn the `i64` into a `u64`.
                    let ret = u64::from_ne_bytes(ret.to_ne_bytes());

                    // According to the runtime environment specifies, the return value is two
                    // consecutive I32s representing the length and size of the SCALE-encoded
                    // return value.
                    let ret_len = u32::try_from(ret >> 32).unwrap();
                    let ret_ptr = u32::try_from(ret & 0xffffffff).unwrap();
                    // TODO: optimization: don't copy memory but immediately decode from slice

                    self.inner.state =
                        if let Ok(ret_data) = self.inner.vm.read_memory(ret_ptr, ret_len) {
                            // We have the raw data, now try to decode it into the proper
                            // strongly-typed return value.
                            let decoded = Success::decode(&self.inner.called_function, &ret_data);

                            match decoded {
                                Ok(v) => StateInner::Finished(v),
                                Err(err) => {
                                    StateInner::NonConforming(NonConformingErr::SuccessDecode(err))
                                }
                            }
                        } else {
                            StateInner::NonConforming(NonConformingErr::ReturnedPtrOutOfRange {
                                pointer: ret_ptr,
                                size: ret_len,
                                memory_size: self.inner.vm.memory_size(),
                            })
                        };

                    break;
                }

                Ok(vm::ExecOutcome::Interrupted { id, params }) => {
                    // The Wasm code has called an externality. The `id` is a value that we passed
                    // at initialization, and corresponds to an index in `registered_functions`.
                    let call_state = self
                        .inner
                        .registered_functions
                        .get_mut(id)
                        .unwrap()
                        .start_call(&params);
                    self.inner.state = StateInner::Calling(call_state);
                }

                Ok(vm::ExecOutcome::Finished {
                    return_value: Ok(_),
                }) => {
                    // The Wasm function has successfully returned, but the specs require that it
                    // returns a `i64`.
                    self.inner.state = StateInner::NonConforming(NonConformingErr::BadReturnValue);
                    break;
                }

                Ok(vm::ExecOutcome::Finished {
                    return_value: Err(()),
                }) => {
                    self.inner.state = StateInner::Trapped;
                    break;
                }

                Err(vm::RunErr::BadValueTy { .. }) => {
                    // We tried to inject back the value returned by an externality, but it
                    // doesn't match what the Wasm code expects.
                    // TODO: check signatures at initialization instead?
                    self.inner.state =
                        StateInner::NonConforming(NonConformingErr::ExternalityBadReturnValue);
                    break;
                }

                Err(vm::RunErr::Poisoned) => {
                    // Can only happen if there's a bug somewhere.
                    unreachable!()
                }
            }
        }
    }
}

pub struct Resume<'a, T> {
    inner: externalities::Resume<'a, T>,
}

impl<'a, T> Resume<'a, T> {
    /// Injects the return value back into the virtual machine and prepares it for continuing to
    /// run.
    ///
    /// > **Note**: This function is lightweight and doesn't perform anything CPU-heavy.
    pub fn finish_call(self, resolve: T) {
        self.inner.inject(resolve);
    }
}

// Glue between the `allocator` module and the `vm` module.
struct MemAccess<'a>(&'a mut vm::VirtualMachine);
impl<'a> allocator::Memory for MemAccess<'a> {
    fn read_le_u64(&self, ptr: u32) -> Result<u64, allocator::Error> {
        let bytes = self.0.read_memory(ptr, 8).unwrap(); // TODO: convert error
        Ok(u64::from_le_bytes(<[u8; 8]>::try_from(&bytes[..]).unwrap()))
    }

    fn write_le_u64(&mut self, ptr: u32, val: u64) -> Result<(), allocator::Error> {
        let bytes = val.to_le_bytes();
        self.0.write_memory(ptr, &bytes).unwrap(); // TODO: convert error instead
        Ok(())
    }

    fn size(&self) -> u32 {
        self.0.memory_size()
    }
}

#[cfg(test)]
mod tests {
    use super::ExternalsVm;

    #[test]
    fn is_send() {
        fn req<T: Send>() {}
        req::<ExternalsVm>();
    }
}
