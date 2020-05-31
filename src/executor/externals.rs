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

use core::convert::TryFrom as _;
use parity_scale_codec::Encode as _;

/// A running virtual machine.
pub struct ExternalsVm {
    /// Inner lower-level virtual machine.
    vm: vm::VirtualMachine,

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
    /// Function isn't known.
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
    Finished(Vec<u8>),
}

/// State of a [`ExternalVm`]. Mutably borrows the virtual machine, thereby ensuring that the
/// state can't change.
pub enum State<'a> {
    ReadyToRun(StateReadyToRun<'a>),
    Finished(&'a [u8]),
    /// The Wasm blob did something that doesn't conform to the runtime environment.
    NonConforming(NonConformingErr),
    /// The Wasm VM has encountered a trap (i.e. it has panicked).
    Trapped,
    ExternalStorageSet {
        // TODO: params
        resolve: StateWaitExternalResolve<'a, ()>,
    },
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
}

pub struct StateReadyToRun<'a> {
    inner: &'a mut ExternalsVm,
}

pub struct StateWaitExternalResolve<'a, T: ?Sized> {
    inner: &'a mut ExternalsVm,
    resume: Box<dyn FnOnce(&mut ExternalsVm, &T) -> Option<vm::RuntimeValue>>,
}

impl ExternalsVm {
    /// Creates a new state machine from the given module that executes the given function.
    pub fn new(module: &vm::WasmBlob, to_call: FunctionToCall) -> Result<Self, vm::NewErr> {
        match to_call {
            FunctionToCall::CoreVersion => ExternalsVm::new_inner(module, "Core_version", &[]),
            FunctionToCall::CoreExecuteBlock(block) => {
                let encoded = block.encode();
                ExternalsVm::new_inner(module, "Core_execute_block", &encoded)
            }
            FunctionToCall::BabeApiConfiguration => {
                ExternalsVm::new_inner(module, "BabeApi_configuration", &[])
            }
            FunctionToCall::GrandpaApiGrandpaAuthorities => {
                ExternalsVm::new_inner(module, "GrandpaApi_grandpa_authorities", &[])
            }
            FunctionToCall::TaggedTransactionsQueueValidateTransaction(extrinsic) => {
                let encoded = extrinsic.encode();
                ExternalsVm::new_inner(
                    module,
                    "TaggedTransactionsQueue_validate_transaction",
                    &encoded,
                )
            }
        }
    }

    /// Creates a new state machine from the given module that executes the given function.
    ///
    /// The `data` parameter is the SCALE-encoded data to inject into the runtime.
    fn new_inner(
        module: &vm::WasmBlob,
        function_name: &str,
        data: &[u8],
    ) -> Result<Self, vm::NewErr> {
        // TODO: write params in memory
        assert!(data.is_empty(), "not implemented");

        let mut registered_functions = Vec::new();
        let vm = vm::VirtualMachine::new(
            module,
            function_name,
            &[vm::RuntimeValue::I32(0), vm::RuntimeValue::I32(0)],
            |mod_name, f_name, signature| {
                if mod_name != "env" {
                    return Err(());
                }

                let id = registered_functions.len();
                registered_functions.push(get_function(f_name));
                Ok(id)
            },
        )?;

        // In the runtime environment, Wasm blobs must export a global symbol named `__heap_base`
        // indicating where the memory allocator is allowed to allocate memory.
        // TODO: this isn't mentioned in the specs but seems mandatory; report to the specs writers
        let heap_base = vm.global_value("__heap_base").unwrap(); // TODO: don't unwrap

        registered_functions.shrink_to_fit();

        Ok(ExternalsVm {
            vm,
            state: StateInner::Ready(None),
            registered_functions,
            allocator: allocator::FreeingBumpHeapAllocator::new(heap_base),
        })
    }

    /// Returns the current state of the virtual machine.
    pub fn state(&mut self) -> State {
        // We have to do that in multiple steps in order to satisfy the borrow checker.
        if let StateInner::Ready(_) = self.state {
            return State::ReadyToRun(StateReadyToRun { inner: self });
        }

        match &self.state {
            StateInner::Ready(_) => unreachable!(),
            StateInner::NonConforming(err) => State::NonConforming(err.clone()),
            StateInner::Trapped => State::Trapped,
            StateInner::Finished(data) => State::Finished(data),
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
                    if let Ok(ret_data) = self.inner.vm.read_memory(ret_ptr, ret_len) {
                        self.inner.state = StateInner::Finished(ret_data);
                    } else {
                        self.inner.state =
                            StateInner::NonConforming(NonConformingErr::ReturnedPtrOutOfRange {
                                pointer: ret_ptr,
                                size: ret_len,
                                memory_size: self.inner.vm.memory_size(),
                            });
                    }
                    break;
                }

                Ok(vm::ExecOutcome::Interrupted { id, params }) => {
                    match self.inner.registered_functions.get_mut(id) {
                        Some(RegisteredFunction::Unresolved { name }) => {
                            self.inner.state = StateInner::NonConforming(
                                NonConformingErr::UnknownFunction(name.clone()),
                            );
                            break;
                        }
                        Some(RegisteredFunction::Immediate { implementation }) => {
                            let ret_val = implementation(
                                &params,
                                &mut self.inner.vm,
                                &mut self.inner.allocator,
                            );
                            self.inner.state = StateInner::Ready(ret_val);
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
    pub fn finish_call(self, resolve: &T) -> StateReadyToRun<'a> {
        self.inner.state = StateInner::Ready((self.resume)(self.inner, resolve));
        StateReadyToRun { inner: self.inner }
    }
}

fn get_function(name: &str) -> RegisteredFunction {
    match name {
        "ext_storage_set_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_get_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_read_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_storage_clear_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
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
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_hashing_twox_128_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
        },
        "ext_hashing_twox_256_version_1" => RegisteredFunction::Immediate {
            implementation: Box::new(|_, _, _| unimplemented!()),
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
