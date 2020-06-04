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

use core::{convert::TryFrom as _, hash::Hasher as _};
use parity_scale_codec::{DecodeAll as _, Encode as _};

/// A running virtual machine.
pub struct ExternalsVm {
    /// Inner lower-level virtual machine.
    vm: vm::VirtualMachine,

    /// Function currently being called.
    called_function: CalledFunction,

    /// State of the virtual machine. Must be in sync with [`ExternalsVm::vm`].
    state: StateInner,

    /// List of functions that the Wasm code imports.
    ///
    /// The keys of this `Vec` (i.e. the `usize` indices) have been passed to the virtual machine
    /// executor. Whenever the Wasm code invokes an external function, we obtain its index, and
    /// look within this `Vec` to know what to do.
    registered_functions: Vec<RegisteredFunction>,

    /// Memory allocator in order to answer the calls to `malloc` and `free`.
    allocator: allocator::FreeingBumpHeapAllocator,
}

/// Which function to call on a runtime blob, and its parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionToCall<'a> {
    CoreVersion,
    CoreExecuteBlock(&'a Block),
    BabeApiConfiguration,
    GrandpaApiGrandpaAuthorities,
    TaggedTransactionsQueueValidateTransaction(&'a [u8]),
    // TODO: add the block builder functions
}

/// Same as [`FunctionToCall`] but without parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CalledFunction {
    CoreVersion,
    CoreExecuteBlock,
    BabeApiConfiguration,
    GrandpaApiGrandpaAuthorities,
    TaggedTransactionsQueueValidateTransaction,
}

impl<'a, 'b> From<&'a FunctionToCall<'b>> for CalledFunction {
    fn from(c: &'a FunctionToCall<'b>) -> Self {
        match c {
            FunctionToCall::CoreVersion => CalledFunction::CoreVersion,
            FunctionToCall::CoreExecuteBlock(_) => CalledFunction::CoreExecuteBlock,
            FunctionToCall::BabeApiConfiguration => CalledFunction::BabeApiConfiguration,
            FunctionToCall::GrandpaApiGrandpaAuthorities => {
                CalledFunction::GrandpaApiGrandpaAuthorities
            }
            FunctionToCall::TaggedTransactionsQueueValidateTransaction(_) => {
                CalledFunction::TaggedTransactionsQueueValidateTransaction
            }
        }
    }
}

enum RegisteredFunction {
    /// The external function performs some immediate action and returns.
    Immediate {
        /// How to execute this function.
        implementation: Box<
            dyn FnMut(
                    &[vm::RuntimeValue],
                    &mut vm::VirtualMachine,
                    &mut allocator::FreeingBumpHeapAllocator,
                ) -> Option<vm::RuntimeValue>
                + Send,
        >,
    },

    /// Function resolution requires an interruption of the virtual machine so that the user can
    /// handle the call.
    Interrupt {
        /// How to execute this function.
        implementation: Box<
            dyn FnMut(
                    &[vm::RuntimeValue],
                    &mut vm::VirtualMachine,
                    &mut allocator::FreeingBumpHeapAllocator,
                ) -> StateInner
                + Send,
        >,
    },

    /// Function isn't known.
    /// In theory we should reject at initialization any runtime code that tries to import a
    /// function that is unknown to this code. In practice, however, it is not unheard to have
    /// runtime code import unknown functions but without calling them. To handle this type of
    /// situation, we accept unknown functions but return an error if they are called.
    Unresolved {
        /// Name of the function.
        name: String,
    },
}

enum StateInner {
    Ready(Option<vm::RuntimeValue>),
    /// The Wasm blob did something that doesn't conform to the runtime environment.
    NonConforming(NonConformingErr),
    /// The Wasm VM has encountered a trap (i.e. it has panicked).
    Trapped,
    ExternalStorageGet {
        storage_key: Vec<u8>,
    },
    ExternalStorageSet {
        storage_key: Vec<u8>,
        new_storage_value: Vec<u8>,
    },
    ExternalStorageClear {
        storage_key: Vec<u8>,
    },
    Finished(Success),
}

/// State of a [`ExternalVm`]. Mutably borrows the virtual machine, thereby ensuring that the
/// state can't change.
pub enum State<'a> {
    ReadyToRun(StateReadyToRun<'a>),
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
        resolve: StateWaitExternalResolve<'a, Option<Vec<u8>>>,
    },
    ExternalStorageSet {
        /// Which key to change.
        storage_key: Vec<u8>,
        /// Which storage value to change.
        new_storage_value: Vec<u8>,
        /// Object to use to finish the call
        resolve: StateWaitExternalResolve<'a, ()>,
    },
    ExternalStorageClear {
        /// Which key to clear.
        storage_key: Vec<u8>,
        /// Object to use to finish the call
        resolve: StateWaitExternalResolve<'a, ()>,
    },
}

/// Contains the same variants as [`FunctionToCall`] but with the strongly-typed sucess value.
#[derive(Debug)]
pub enum Success {
    CoreVersion(CoreVersionSuccess),
    CoreExecuteBlock(bool),
    // TODO: other functions
}

#[derive(Debug, Clone, PartialEq, Eq, parity_scale_codec::Encode, parity_scale_codec::Decode)]
pub struct CoreVersionSuccess {
    pub spec_name: String,
    pub impl_name: String,
    pub authoring_version: u32,
    pub spec_version: u32,
    pub impl_version: u32,
    // TODO: specs document this as an unexplained `ApisVec`; report that to specs writer
    // TODO: stronger typing
    pub apis: Vec<([u8; 8], u32)>,
    // TODO: this field has been introduced recently and isn't mention in the specs; report that to specs writer
    pub transaction_version: u32,
}

/// Reason why the Wasm blob isn't conforming to the runtime environment.
#[derive(Debug, Clone, derive_more::Display)]
pub enum NonConformingErr {
    /// An unknown function has been called.
    #[display(fmt = "An unknown function has been called: {}", _0)]
    UnknownFunction(String),
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
}

pub struct StateReadyToRun<'a> {
    inner: &'a mut ExternalsVm,
}

pub struct StateWaitExternalResolve<'a, T: ?Sized> {
    inner: &'a mut ExternalsVm,
    resume: Box<dyn FnOnce(&mut ExternalsVm, T) -> Option<vm::RuntimeValue>>,
}

impl ExternalsVm {
    /// Creates a new state machine from the given module that executes the given function.
    pub fn new(module: &vm::WasmBlob, to_call: FunctionToCall) -> Result<Self, vm::NewErr> {
        let called_function = CalledFunction::from(&to_call);

        match to_call {
            FunctionToCall::CoreVersion => ExternalsVm::new_inner(module, called_function, &[]),
            FunctionToCall::CoreExecuteBlock(block) => {
                let encoded = block.encode();
                ExternalsVm::new_inner(module, called_function, &encoded)
            }
            FunctionToCall::BabeApiConfiguration => {
                ExternalsVm::new_inner(module, called_function, &[])
            }
            FunctionToCall::GrandpaApiGrandpaAuthorities => {
                ExternalsVm::new_inner(module, called_function, &[])
            }
            FunctionToCall::TaggedTransactionsQueueValidateTransaction(extrinsic) => {
                let encoded = extrinsic.encode();
                ExternalsVm::new_inner(module, called_function, &encoded)
            }
        }
    }

    /// Creates a new state machine from the given module that executes the given function.
    ///
    /// The `data` parameter is the SCALE-encoded data to inject into the runtime.
    fn new_inner(
        module: &vm::WasmBlob,
        called_function: CalledFunction,
        data: &[u8],
    ) -> Result<Self, vm::NewErr> {
        let function_name = match called_function {
            CalledFunction::CoreVersion => "Core_version",
            CalledFunction::CoreExecuteBlock => "Core_execute_block",
            CalledFunction::BabeApiConfiguration => "BabeApi_configuration",
            CalledFunction::GrandpaApiGrandpaAuthorities => "GrandpaApi_grandpa_authorities",
            CalledFunction::TaggedTransactionsQueueValidateTransaction => {
                "TaggedTransactionsQueue_validate_transaction"
            }
        };

        // TODO: error if input data is too large?

        let mut registered_functions = Vec::new();
        let mut vm = vm::VirtualMachine::new(
            module,
            function_name,
            &[
                vm::RuntimeValue::I32(0),
                vm::RuntimeValue::I32(i32::try_from(data.len()).unwrap()),
            ],
            |mod_name, f_name, _signature| {
                if mod_name != "env" {
                    return Err(());
                }

                let id = registered_functions.len();
                registered_functions.push(get_function(f_name));
                Ok(id)
            },
        )?;
        registered_functions.shrink_to_fit();

        // Initialize the state of the memory allocator. This is the allocator that is later used
        // when the Wasm code requests variable-length data.
        let mut allocator = {
            // In the runtime environment, Wasm blobs must export a global symbol named
            // `__heap_base` indicating where the memory allocator is allowed to allocate memory.
            // TODO: this isn't mentioned in the specs but seems mandatory; report to the specs writers
            let mut heap_base = vm.global_value("__heap_base").unwrap(); // TODO: don't unwrap
            allocator::FreeingBumpHeapAllocator::new(heap_base)
        };

        // Use that allocator to write the input data.
        // This has the consequence that the user is allowed to free that input data. If this
        // consequence is unwanted, this code can be changed to write the input data at `heap_base`
        // then have the actual heap after the input data.
        // TODO: don't unwrap
        let input_data_location = allocator
            .allocate(&mut MemAccess(&mut vm), u32::try_from(data.len()).unwrap())
            .unwrap();
        vm.write_memory(input_data_location, data).unwrap();

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
        // We have to do that in multiple steps in order to satisfy the borrow checker.
        if let StateInner::Ready(_) = self.state {
            return State::ReadyToRun(StateReadyToRun { inner: self });
        }

        if let StateInner::ExternalStorageGet { storage_key } = &self.state {
            return State::ExternalStorageGet {
                storage_key: storage_key.clone(), // TODO: don't clone
                resolve: StateWaitExternalResolve {
                    inner: self,
                    resume: Box::new(|this, value| {
                        let data = value.encode(); // TODO: clones the value
                        let data_len_u32 = u32::try_from(data.len()).unwrap(); // TODO: don't unwrap
                        let ret = this
                            .allocator
                            .allocate(&mut MemAccess(&mut this.vm), data_len_u32)
                            .unwrap(); // TODO: don't unwrap
                        this.vm.write_memory(ret, &data).unwrap();
                        let ret = (u64::from(data_len_u32) << 32) | u64::from(ret);
                        Some(vm::RuntimeValue::I64(i64::from_ne_bytes(ret.to_ne_bytes())))
                    }),
                },
            };
        }

        if let StateInner::ExternalStorageSet {
            storage_key,
            new_storage_value,
        } = &self.state
        {
            return State::ExternalStorageSet {
                storage_key: storage_key.clone(),             // TODO: don't clone
                new_storage_value: new_storage_value.clone(), // TODO: don't clone
                resolve: StateWaitExternalResolve {
                    inner: self,
                    resume: Box::new(|_this, ()| None),
                },
            };
        }

        if let StateInner::ExternalStorageClear { storage_key } = &self.state {
            return State::ExternalStorageClear {
                storage_key: storage_key.clone(), // TODO: don't clone
                resolve: StateWaitExternalResolve {
                    inner: self,
                    resume: Box::new(|_this, ()| None),
                },
            };
        }

        match &self.state {
            StateInner::Ready(_) => unreachable!(),
            StateInner::NonConforming(err) => State::NonConforming(err.clone()),
            StateInner::Trapped => State::Trapped,
            StateInner::Finished(data) => State::Finished(data),
            _ => unreachable!(),
        }
    }
}

impl<'a> From<StateReadyToRun<'a>> for State<'a> {
    fn from(state: StateReadyToRun<'a>) -> State<'a> {
        State::ReadyToRun(state)
    }
}

impl<'a> StateReadyToRun<'a> {
    /// Runs the virtual machine until something important happens.
    ///
    /// > **Note**: This is when the actual CPU-heavy computation happens.
    pub fn run(self) -> State<'a> {
        loop {
            let resume_value = match &mut self.inner.state {
                StateInner::Ready(val) => val.take(),
                _ => unreachable!(),
            };

            match self.inner.vm.run(resume_value) {
                Ok(vm::ExecOutcome::Finished {
                    return_value: Ok(Some(vm::RuntimeValue::I64(ret))),
                }) => {
                    // Turn the `i64` into a `u64`.
                    let ret = u64::from_ne_bytes(ret.to_ne_bytes());

                    // According to the runtime environment specifies, the return value is two
                    // consecutive I32s representing the length and size of the SCALE-encoded
                    // return value.
                    let ret_len = u32::try_from(ret >> 32).unwrap();
                    let ret_ptr = u32::try_from(ret & 0xffffffff).unwrap();
                    // TODO: optimization: don't copy memory but immediately decode from slice

                    self.inner.state = if let Ok(ret_data) =
                        self.inner.vm.read_memory(ret_ptr, ret_len)
                    {
                        // We have the raw data, now try to decode it into the proper
                        // strongly-typed return value.
                        let decoded = match self.inner.called_function {
                            CalledFunction::CoreVersion => {
                                CoreVersionSuccess::decode_all(&ret_data).map(Success::CoreVersion)
                            }
                            CalledFunction::CoreExecuteBlock => {
                                bool::decode_all(&ret_data).map(Success::CoreExecuteBlock)
                            }
                            _ => unimplemented!("return value for this function not implemented"), // TODO:
                        };

                        match decoded {
                            Ok(v) => StateInner::Finished(v),
                            Err(err) => StateInner::NonConforming(
                                NonConformingErr::ReturnedValueDecodeFail(err),
                            ),
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
                    match self.inner.registered_functions.get_mut(id) {
                        Some(RegisteredFunction::Immediate { implementation }) => {
                            let ret_val = implementation(
                                &params,
                                &mut self.inner.vm,
                                &mut self.inner.allocator,
                            );
                            self.inner.state = StateInner::Ready(ret_val);
                        }
                        Some(RegisteredFunction::Interrupt { implementation }) => {
                            let new_state = implementation(
                                &params,
                                &mut self.inner.vm,
                                &mut self.inner.allocator,
                            );
                            self.inner.state = new_state;
                            break;
                        }
                        Some(RegisteredFunction::Unresolved { name }) => {
                            self.inner.state = StateInner::NonConforming(
                                NonConformingErr::UnknownFunction(name.clone()),
                            );
                            break;
                        }

                        // Internal error. We have been passed back an `id` that we didn't return
                        // during the imports resolution.
                        None => panic!(),
                    }
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

                Err(err) => panic!("Internal error in Wasm virtual machine: {}", err),
            }
        }

        self.inner.state()
    }
}

impl<'a, T> StateWaitExternalResolve<'a, T> {
    /// Injects the return value back into the virtual machine and prepares it for continuing to
    /// run.
    ///
    /// > **Note**: This function is lightweight and doesn't perform anything CPU-heavy.
    pub fn finish_call(self, resolve: T) -> StateReadyToRun<'a> {
        self.inner.state = StateInner::Ready((self.resume)(self.inner, resolve));
        StateReadyToRun { inner: self.inner }
    }
}

// TODO: should somehow filter out categories of functions, so that offchain worker functions
// are for example forbidden for regular blocks
fn get_function(name: &str) -> RegisteredFunction {
    match name {
        "ext_storage_set_version_1" => RegisteredFunction::Interrupt {
            implementation: Box::new(|params, vm, _alloc| {
                // TODO: check params count
                let storage_key = expect_ptr_len(&params[0], vm);
                let new_storage_value = expect_ptr_len(&params[1], vm);
                StateInner::ExternalStorageSet {
                    storage_key,
                    new_storage_value,
                }
            }),
        },
        "ext_storage_get_version_1" => RegisteredFunction::Interrupt {
            implementation: Box::new(|params, vm, _alloc| {
                let storage_key = expect_one_ptr_len(&params, vm);
                StateInner::ExternalStorageGet { storage_key }
            }),
        },
        "ext_storage_read_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_clear_version_1" => RegisteredFunction::Interrupt {
            implementation: Box::new(|params, vm, _alloc| {
                let storage_key = expect_one_ptr_len(&params, vm);
                StateInner::ExternalStorageClear { storage_key }
            }),
        },
        "ext_storage_exists_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_clear_prefix_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_root_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_changes_root_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_next_key_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_child_set_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_child_get_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_child_read_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_child_clear_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_child_storage_kill_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_child_exists_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_child_clear_prefix_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_child_root_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_child_next_key_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_crypto_ed25519_public_keys_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_crypto_ed25519_generate_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_crypto_ed25519_sign_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_crypto_ed25519_verify_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_crypto_sr25519_public_keys_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_crypto_sr25519_generate_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_crypto_sr25519_sign_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_crypto_sr25519_verify_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_crypto_secp256k1_ecdsa_recover_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_crypto_secp256k1_ecdsa_recover_compressed_version_1" => {
            RegisteredFunction::Immediate {
                implementation: Box::new(|_, _, _| unimplemented!()),
            }
        }
        "ext_hashing_keccak_256_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_hashing_sha2_256_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_hashing_blake2_128_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_hashing_blake2_256_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_hashing_twox_64_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|params, vm, alloc| {
                let data = expect_one_ptr_len(&params, vm);

                let mut h0 = twox_hash::XxHash::with_seed(0);
                h0.write(&data);
                let r0 = h0.finish();

                let ret = alloc.allocate(&mut MemAccess(vm), 8).unwrap(); // TODO: don't unwrap
                vm.write_memory(ret, &r0.to_le_bytes()).unwrap();
                Some(vm::RuntimeValue::I32(i32::try_from(ret).unwrap())) // TODO: don't unwrap
            }),
        },
        "ext_hashing_twox_128_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|params, vm, alloc| {
                let data = expect_one_ptr_len(&params, vm);

                let mut h0 = twox_hash::XxHash::with_seed(0);
                let mut h1 = twox_hash::XxHash::with_seed(1);
                h0.write(&data);
                h1.write(&data);
                let r0 = h0.finish();
                let r1 = h1.finish();

                let ret = alloc.allocate(&mut MemAccess(vm), 16).unwrap(); // TODO: don't unwrap
                vm.write_memory(ret, &r0.to_le_bytes()).unwrap();
                vm.write_memory(ret + 8, &r1.to_le_bytes()).unwrap();
                Some(vm::RuntimeValue::I32(i32::try_from(ret).unwrap())) // TODO: don't unwrap
            }),
        },
        "ext_hashing_twox_256_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|params, vm, alloc| {
                let data = expect_one_ptr_len(&params, vm);

                let mut h0 = twox_hash::XxHash::with_seed(0);
                let mut h1 = twox_hash::XxHash::with_seed(1);
                let mut h2 = twox_hash::XxHash::with_seed(2);
                let mut h3 = twox_hash::XxHash::with_seed(3);
                h0.write(&data);
                h1.write(&data);
                h2.write(&data);
                h3.write(&data);
                let r0 = h0.finish();
                let r1 = h1.finish();
                let r2 = h2.finish();
                let r3 = h3.finish();

                let ret = alloc.allocate(&mut MemAccess(vm), 32).unwrap(); // TODO: don't unwrap
                vm.write_memory(ret, &r0.to_le_bytes()).unwrap();
                vm.write_memory(ret + 8, &r1.to_le_bytes()).unwrap();
                vm.write_memory(ret + 16, &r2.to_le_bytes()).unwrap();
                vm.write_memory(ret + 24, &r3.to_le_bytes()).unwrap();
                Some(vm::RuntimeValue::I32(i32::try_from(ret).unwrap())) // TODO: don't unwrap
            }),
        },
        "ext_offchain_is_validator_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_submit_transaction_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_network_state_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_timestamp_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_sleep_until_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_random_seed_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_local_storage_set_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_local_storage_compare_and_set_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_local_storage_get_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_http_request_start_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_http_request_add_header_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_http_request_write_body_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_http_response_wait_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_http_response_headers_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_offchain_http_response_read_body_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_trie_blake2_256_root_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_trie_blake2_256_ordered_root_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_misc_chain_id_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_misc_print_num_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_misc_print_utf8_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_misc_print_hex_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_misc_runtime_version_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_allocator_malloc_version_1" => {
            RegisteredFunction::Immediate {
                implementation: Box::new(move |params, vm, alloc| {
                    let param = match params[0] {
                        vm::RuntimeValue::I32(v) => u32::try_from(v).unwrap(), // TODO: don't unwrap
                        _ => panic!(),                                         // TODO:
                    };

                    let ret = alloc.allocate(&mut MemAccess(vm), param).unwrap(); // TODO: don't unwrap
                    Some(vm::RuntimeValue::I32(i32::try_from(ret).unwrap())) // TODO: don't unwrap
                }),
            }
        }
        "ext_allocator_free_version_1" => {
            RegisteredFunction::Immediate {
                implementation: Box::new(move |params, vm, alloc| {
                    let param = match params[0] {
                        vm::RuntimeValue::I32(v) => u32::try_from(v).unwrap(), // TODO: don't unwrap
                        _ => panic!(),                                         // TODO:
                    };

                    alloc.deallocate(&mut MemAccess(vm), param).unwrap(); // TODO: don't unwrap
                    None
                }),
            }
        }
        "ext_logging_log_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        _ => RegisteredFunction::Unresolved {
            name: name.to_owned(),
        },
    }
}

// TODO: document and all
// TODO: don't panic, return a result
// TODO: should ideally not copy the data
fn expect_one_ptr_len(params: &[vm::RuntimeValue], vm: &mut vm::VirtualMachine) -> Vec<u8> {
    assert_eq!(params.len(), 1);
    expect_ptr_len(&params[0], vm)
}

// TODO: document and all
// TODO: don't panic, return a result
// TODO: should ideally not copy the data
fn expect_ptr_len(param: &vm::RuntimeValue, vm: &mut vm::VirtualMachine) -> Vec<u8> {
    let val = match param {
        vm::RuntimeValue::I64(v) => u64::from_ne_bytes(v.to_ne_bytes()),
        _ => panic!(),
    };

    let len = u32::try_from(val >> 32).unwrap();
    let ptr = u32::try_from(val & 0xffffffff).unwrap();

    vm.read_memory(ptr, len).unwrap()
}

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
