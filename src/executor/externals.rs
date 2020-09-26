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
//!
//! Contrary to most programs, Wasm runtime code doesn't have a singe `main` function. Instead, it
//! exposes several entry points. Which one to call indicates which action it has to perform. Not
//! all entry points are necessarily available on all runtimes.
//!
//! # ABI
//!
//! All entry points have the same signature:
//!
//! ```ignore
//! (func $runtime_entry(param $data i32) (param $len i32) (result i64))
//! ```
//!
//! In order to call into the runtime, one must write a buffer of data containing the input
//! parameters into the Wasm virtual machine's memory, then pass a pointer and length of this
//! buffer as the parameters of the entry point.
//!
//! The function returns a 64bits number. The 32 less significant bits represent a pointer to the
//! Wasm virtual machine's memory, and the 32 most significant bits a length. This pointer and
//! length designate a buffer containing the actual return value.

use super::{allocator, vm};

use core::{convert::TryFrom as _, fmt, iter, mem};

mod externalities;

/// Prototype for an [`ExternalsVm`].
pub struct ExternalsVmPrototype {
    /// Inner virtual machine prototype.
    vm_proto: vm::VirtualMachinePrototype,

    /// Initial value of the `__heap_base` global in the Wasm module. Used to initialize the memory
    /// allocator.
    heap_base: u32,

    /// List of functions that the Wasm code imports.
    ///
    /// The keys of this `Vec` (i.e. the `usize` indices) have been passed to the virtual machine
    /// executor. Whenever the Wasm code invokes an external function, we obtain its index, and
    /// look within this `Vec` to know what to do.
    registered_functions: Vec<externalities::Externality>,
}

impl ExternalsVmPrototype {
    /// Creates a new [`ExternalsVmPrototype`]. Parses and potentially JITs the module.
    // TODO: document `heap_pages`; I know it comes from storage, but it's unclear what it means exactly
    pub fn new(module: impl AsRef<[u8]>, heap_pages: u64) -> Result<Self, NewErr> {
        // Initialize the virtual machine.
        // Each symbol requested by the Wasm runtime will be put in `registered_functions`. Later,
        // when a function is invoked, the Wasm virtual machine will pass indices within that
        // array.
        let (vm_proto, registered_functions) = {
            let mut registered_functions = Vec::new();
            let vm_proto = vm::VirtualMachinePrototype::new(
                module,
                heap_pages,
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
            (vm_proto, registered_functions)
        };

        // In the runtime environment, Wasm blobs must export a global symbol named
        // `__heap_base` indicating where the memory allocator is allowed to allocate memory.
        let heap_base = vm_proto
            .global_value("__heap_base")
            .map_err(|_| NewErr::HeapBaseNotFound)?;

        Ok(ExternalsVmPrototype {
            vm_proto,
            heap_base,
            registered_functions,
        })
    }

    /// Starts the VM, calling the function passed as parameter.
    pub fn run(self, function_to_call: &str, data: &[u8]) -> Result<ExternalsVm, NewErr> {
        self.run_vectored(function_to_call, iter::once(data))
    }

    /// Same as [`ExternalsVmPrototype::run`], except that the function desn't need any parameter.
    pub fn run_no_param(self, function_to_call: &str) -> Result<ExternalsVm, NewErr> {
        self.run_vectored(function_to_call, iter::empty::<Vec<u8>>())
    }

    /// Same as [`ExternalsVmPrototype::run`], except that the function parameter can be passed as
    /// a list of buffers. All the buffers will be concatenated in memory.
    pub fn run_vectored(
        self,
        function_to_call: &str,
        data: impl Iterator<Item = impl AsRef<[u8]>> + Clone,
    ) -> Result<ExternalsVm, NewErr> {
        let mut data_len_u32: u32 = 0;
        for data in data.clone() {
            let len = u32::try_from(data.as_ref().len()).map_err(|_| NewErr::DataSizeOverflow)?;
            data_len_u32 = data_len_u32
                .checked_add(len)
                .ok_or(NewErr::DataSizeOverflow)?;
        }

        // Now create the actual virtual machine. We pass as parameter `heap_base` as the location
        // of the input data.
        let mut vm = self.vm_proto.start(
            function_to_call,
            &[
                vm::WasmValue::I32(i32::from_ne_bytes(self.heap_base.to_ne_bytes())),
                vm::WasmValue::I32(i32::from_ne_bytes(data_len_u32.to_ne_bytes())),
            ],
        )?;

        // Now writing the input data into the VM.
        let mut after_input_data = self.heap_base;
        for data in data {
            let data = data.as_ref();
            vm.write_memory(after_input_data, data).unwrap();
            after_input_data = after_input_data
                .checked_add(u32::try_from(data.len()).unwrap())
                .unwrap();
        }

        // Initialize the state of the memory allocator. This is the allocator that is later used
        // when the Wasm code requests variable-length data.
        let allocator = allocator::FreeingBumpHeapAllocator::new(after_input_data);

        Ok(ExternalsVm {
            vm,
            heap_base: self.heap_base,
            state: StateInner::Ready(None),
            registered_functions: self.registered_functions,
            allocator,
        })
    }
}

/// A running virtual machine.
pub struct ExternalsVm {
    /// Inner lower-level virtual machine.
    vm: vm::VirtualMachine,

    /// Initial value of the `__heap_base` global in the Wasm module. Used to initialize the memory
    /// allocator in case we need to rebuild the VM.
    heap_base: u32,

    /// State of the virtual machine. Must be in sync with [`ExternalsVm::vm`].
    state: StateInner,

    /// See [`ExternalsVmPrototype::registered_functions`].
    registered_functions: Vec<externalities::Externality>,

    /// Memory allocator in order to answer the calls to `malloc` and `free`.
    allocator: allocator::FreeingBumpHeapAllocator,
}

/// State of the virtual machine.
enum StateInner {
    /// Wasm virtual machine is ready to be run. Will pass the first parameter as the "resume"
    /// value. This is either `None` at initialization, or, if the virtual machine was calling
    /// an externality, the value returned by the externality.
    Ready(Option<vm::WasmValue>),
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
    Finished(Vec<u8>),
    /// Temporary state to permit state transitions without running into borrowing issues.
    Poisoned,
}

impl ExternalsVm {
    /// Returns the current state of the virtual machine.
    pub fn state(&mut self) -> State {
        // Note: the internal structure of this function is unfortunately a bit weird because we
        // need to bypass limitations in the borrow checker. Ideally, we would use a single big
        // match block inside of a loop.

        // First, let's make the internal state progress, if possible.
        while matches!(self.state, StateInner::Calling(_)) {
            // In order to satisfy the borrow checker, we extract the call state and replace it
            // with `Poisoned`. Below, we put back a proper value.
            let mut calling = match mem::replace(&mut self.state, StateInner::Poisoned) {
                StateInner::Calling(calling) => calling,
                _ => unreachable!(),
            };

            match calling.run() {
                externalities::State::Finished { return_value } => {
                    // The call has finished, meaning that we are ready to resume the
                    // Wasm code.
                    self.state = StateInner::Ready(return_value)
                }
                externalities::State::Error(err) => unimplemented!("{:?}", err), // TODO:

                // The variants below can be handled immediately, and then we continue looping.
                externalities::State::AllocationNeeded { size, inject_value } => {
                    self.state = match self.allocator.allocate(&mut MemAccess(&mut self.vm), size) {
                        Ok(ptr) => {
                            inject_value.inject(ptr);
                            StateInner::Calling(calling)
                        }
                        Err(err) => {
                            println!("allocation size {} failed: {:?}", size, err); // TODO: no
                            StateInner::Trapped
                        }
                    };
                }
                externalities::State::UntrustedDealloc { pointer, done } => {
                    self.state = if self
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
                    self.state = if let Ok(data) = self.vm.read_memory(offset, size) {
                        // TODO: to_vec() overhead
                        inject_value.inject(data.as_ref().to_vec());
                        StateInner::Calling(calling)
                    } else {
                        StateInner::Trapped
                    }
                }
                externalities::State::MemoryWriteNeeded { offset, data, done } => {
                    self.state = if self.vm.write_memory(offset, data).is_ok() {
                        done.inject(());
                        StateInner::Calling(calling)
                    } else {
                        StateInner::Trapped
                    }
                }

                // Other non-handled variants cannot be handled immediately and require a user
                // intervention. We break from the loop and do that below.
                _ => {
                    self.state = StateInner::Calling(calling);
                    break;
                }
            }
        }

        // Sanity check.
        assert!(!matches!(self.state, StateInner::Poisoned));

        // At this point of the function, the internal state cannot progress anymore without user
        // intervention. All the paths below return a `State`.

        // We put this one separately because of borrowing issues.
        if let StateInner::Ready(_) = self.state {
            return State::ReadyToRun(ReadyToRun { inner: self });
        }

        match &mut self.state {
            StateInner::Ready(_) | StateInner::Poisoned => unreachable!(),
            StateInner::NonConforming(err) => State::NonConforming(err.clone()),
            StateInner::Trapped => State::Trapped,
            StateInner::Finished(success) => State::Finished(success),
            StateInner::Calling(calling) => {
                match calling.run() {
                    externalities::State::StorageGetNeeded {
                        key,
                        offset,
                        max_size,
                        done,
                    } => {
                        return State::ExternalStorageGet {
                            storage_key: key,
                            offset,
                            max_size,
                            resolve: Resume { inner: done },
                        }
                    }
                    externalities::State::StorageSetNeeded { key, value, done } => {
                        return State::ExternalStorageSet {
                            storage_key: key,
                            new_storage_value: value,
                            resolve: Resume { inner: done },
                        }
                    }
                    externalities::State::StorageAppendNeeded { key, value, done } => {
                        return State::ExternalStorageAppend {
                            storage_key: key,
                            value,
                            resolve: Resume { inner: done },
                        }
                    }
                    externalities::State::StorageClearPrefixNeeded { key, done } => {
                        return State::ExternalStorageClearPrefix {
                            storage_key: key,
                            resolve: Resume { inner: done },
                        }
                    }
                    externalities::State::StorageRootNeeded { done } => {
                        return State::ExternalStorageRoot {
                            resolve: Resume { inner: done },
                        }
                    }
                    externalities::State::StorageChangesRootNeeded { parent_hash, done } => {
                        return State::ExternalStorageChangesRoot {
                            parent_hash,
                            resolve: Resume { inner: done },
                        }
                    }
                    externalities::State::StorageNextKeyNeeded { key, done } => {
                        return State::ExternalStorageNextKey {
                            storage_key: key,
                            resolve: Resume { inner: done },
                        }
                    }
                    externalities::State::OffchainStorageSetNeeded { key, value, done } => {
                        return State::ExternalOffchainStorageSet {
                            storage_key: key,
                            new_storage_value: value,
                            resolve: Resume { inner: done },
                        }
                    }
                    externalities::State::CallRuntimeVersionNeeded { wasm_blob, done } => {
                        return State::CallRuntimeVersion {
                            wasm_blob,
                            resolve: Resume { inner: done },
                        }
                    }
                    externalities::State::LogEmit { message, done } => {
                        return State::LogEmit {
                            message,
                            resolve: Resume { inner: done },
                        }
                    }

                    // These variants are handled above.
                    externalities::State::Finished { .. }
                    | externalities::State::Error { .. }
                    | externalities::State::AllocationNeeded { .. }
                    | externalities::State::UntrustedDealloc { .. }
                    | externalities::State::MemoryReadNeeded { .. }
                    | externalities::State::MemoryWriteNeeded { .. } => unreachable!(),
                }
            }
        }
    }

    /// Turns the virtual machine back into a prototype.
    pub fn into_prototype(self) -> ExternalsVmPrototype {
        ExternalsVmPrototype {
            vm_proto: self.vm.into_prototype(),
            heap_base: self.heap_base,
            registered_functions: self.registered_functions,
        }
    }
}

/// State of an [`ExternalsVm`]. Mutably borrows the virtual machine, thereby ensuring that the
/// state can't change.
#[derive(Debug)]
pub enum State<'a> {
    /// Wasm virtual machine is ready to be run. Call [`ReadyToRun::run`] to make progress.
    ReadyToRun(ReadyToRun<'a>),
    /// Function execution has succeeded. Contains the return value of the call.
    Finished(&'a [u8]),
    /// The Wasm blob did something that doesn't conform to the runtime environment.
    NonConforming(NonConformingErr),
    /// The Wasm VM has encountered a trap (i.e. it has panicked).
    Trapped,
    ExternalStorageGet {
        /// Which key is requested.
        storage_key: &'a [u8],
        /// Offset in the value where to start reading.
        offset: u32,
        /// Maximum size of the value to return.
        max_size: u32,
        /// Object to use to inject the storage value back. Pass back `None` if key is missing
        /// from storage. Must never be longer than `max_size`.
        resolve: Resume<'a, Option<Vec<u8>>>,
    },
    ExternalStorageSet {
        /// Which key to change.
        storage_key: &'a [u8],
        /// Which storage value to set. `None` if the value must be removed.
        new_storage_value: Option<&'a [u8]>,
        /// Object to use to finish the call
        resolve: Resume<'a, ()>,
    },
    /// Must load a storage value, treat it as if it was a SCALE-encoded container, and put `value`
    /// at the end of the container, increasing the number of elements.
    ///
    /// If there isn't any existing value of if the existing value isn't actually a SCALE-encoded
    /// container, store a 1-size container with the `value`.
    ///
    /// # Details
    ///
    /// The SCALE encoding encodes containers as a SCALE-compact-encoded length followed with the
    /// SCALE-encoded items one after the other. For example, a container of two elements is stored
    /// as the number `2` followed with the two items.
    ///
    /// This change consists in taking an existing value and assuming that it is a SCALE-encoded
    /// container. This can be done as decoding a SCALE-compact-encoded number at the start of
    /// the existing encoded value. One most then increment that number and puting `value` at the
    /// end of the encoded value.
    ///
    /// It is not necessary to decode `value` as is assumed that is already encoded in the same
    /// way as the other items in the container.
    ExternalStorageAppend {
        /// Which key to change.
        storage_key: &'a [u8],
        /// Item to append to the storage value.
        value: &'a [u8],
        /// Object to use to finish the call
        resolve: Resume<'a, ()>,
    },
    ExternalStorageClearPrefix {
        /// Which key to clear.
        storage_key: &'a [u8],
        /// Object to use to finish the call
        resolve: Resume<'a, ()>,
    },
    ExternalStorageRoot {
        /// Object to use to finish the call
        resolve: Resume<'a, [u8; 32]>,
    },
    ExternalStorageChangesRoot {
        parent_hash: &'a [u8],
        /// Object to use to finish the call
        resolve: Resume<'a, Option<[u8; 32]>>,
    },
    ExternalStorageNextKey {
        /// Concerned key.
        storage_key: &'a [u8],
        /// Object to use to finish the call
        resolve: Resume<'a, Option<Vec<u8>>>,
    },
    ExternalOffchainStorageSet {
        /// Which key to change.
        storage_key: &'a [u8],
        /// Which value to set. `None` if the value must be removed.
        new_storage_value: Option<&'a [u8]>,
        /// Object to use to finish the call
        resolve: Resume<'a, ()>,
    },
    /// Need to call `Core_version` on the given Wasm code and return the raw output (i.e.
    /// still SCALE-encoded), or an error if the call has failed.
    CallRuntimeVersion {
        /// Wasm blob to compile and run.
        wasm_blob: &'a [u8],
        /// Object to use to finish the call.
        resolve: Resume<'a, Result<Vec<u8>, ()>>,
    },
    /// Runtime has emitted a log entry.
    LogEmit {
        /// Message emitted by the runtime.
        message: &'a str,
        /// Object to use to finish the call.
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
        // TODO: the purpose of this loop is that the next time the user calls `state()`, they
        // don't get a `ReadyToRun` again. However this will happen in practice if the Wasm code
        // calls an externality that doesn't need any user intervention.
        // TODO: at the moment, this loop never actually loops, all paths reach `break`
        loop {
            // This object can only exist is the state is "ready". We extract the value inside to
            // pass it to the inner state machine.
            let resume_value = match &mut self.inner.state {
                StateInner::Ready(val) => val.take(),
                _ => unreachable!(),
            };

            match self.inner.vm.run(resume_value) {
                Ok(vm::ExecOutcome::Finished {
                    return_value: Ok(Some(vm::WasmValue::I64(ret))),
                }) => {
                    // Wasm virtual machine has successfully returned.

                    // TODO: rewrite this code to be cleaner.
                    // Turn the `i64` into a `u64`.
                    let ret = u64::from_ne_bytes(ret.to_ne_bytes());

                    // According to the runtime environment specifications, the return value is two
                    // consecutive I32s representing the length and size of the SCALE-encoded
                    // return value.
                    let ret_len = u32::try_from(ret >> 32).unwrap();
                    let ret_ptr = u32::try_from(ret & 0xffffffff).unwrap();

                    // TODO: optimization: don't copy memory?
                    self.inner.state =
                        if let Ok(ret_data) = self.inner.vm.read_memory(ret_ptr, ret_len) {
                            StateInner::Finished(ret_data.as_ref().to_vec()) // TODO: overhead
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
                    let externality = self.inner.registered_functions.get_mut(id).unwrap();
                    let call_state = externality.start_call(&params);
                    self.inner.state = StateInner::Calling(call_state);
                    break;
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

impl<'a> fmt::Debug for ReadyToRun<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("ReadyToRun").finish()
    }
}

/// Returned as part of some variants of [`State`]. Allows injecting the result of the requested
/// operation.
pub struct Resume<'a, T> {
    inner: externalities::Resume<'a, T>,
}

impl<'a, T> Resume<'a, T> {
    /// Injects the return value back into the virtual machine and prepares it for continuing to
    /// run.
    ///
    /// > **Note**: This function is lightweight and doesn't perform any CPU-heavy operation.
    pub fn finish_call(self, resolve: T) {
        self.inner.inject(resolve);
    }
}

impl<'a, T> fmt::Debug for Resume<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Resume").finish()
    }
}

// Glue between the `allocator` module and the `vm` module.
struct MemAccess<'a>(&'a mut vm::VirtualMachine);
impl<'a> allocator::Memory for MemAccess<'a> {
    fn read_le_u64(&self, ptr: u32) -> Result<u64, allocator::Error> {
        let bytes = self.0.read_memory(ptr, 8).unwrap(); // TODO: convert error
        Ok(u64::from_le_bytes(
            <[u8; 8]>::try_from(bytes.as_ref()).unwrap(),
        ))
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
