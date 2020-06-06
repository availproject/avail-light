//! Registry of functions available to the Wasm runtime.
//!
//! # Overview
//!
//! Wasm runtimes have functions are their disposal, commonly called "externalities". An
//! "externality" is nothing more than a function that the Wasm code can import and call.
//!
//! Some of these externalities exist because implementing the equivalent behaviour in Wasm would
//! degrade performances. For example, the `ext_hashing_sha2_256_version_1` function computes
//! the SHA256 hash of the data passed as parameter. While it could be possible to put the SHA256
//! hashing algorithm directly in the Wasm runtime, it is in practice much faster to ask the host
//! to compute said hash.
//!
//! Other externalities require an intervention from the user. For example, the
//! `ext_storage_set_version_1` function involves writing data in the storage, which is out of
//! scope of this module.
//!
//! # Calling an externality
//!
//! How to use the code in this module:
//!
//! - Call [`function_by_name`] to parse an externality's name and obtain an [`Externality`].
//!
//! - When the Wasm runtime calls said externality, call [`Externality::start_call`]. This creates
//! a state machine that holds the progress of the call. This state machine will be kept in sync
//! with the actual state of the Wasm VM and drives the process.
//!
//! - Call [`CallState::run`] to progress the call and obtain the next action required to be
//! performed.
//!
//! - If [`State::Finished`] is returned, the call is finished and the [`CallState`] can be
//! destroyed.
//!

// # Implementation notes
//
// The API provided by this module is designed to be easy to implement using generators.
// Unfortunately, generators aren't a stable Rust feature as of the time of the writing of this
// comment. We're instead using async/await, which works in the same way as generators, but does
// not permit zero-cost abstractions in this situation.

use super::vm;

use alloc::sync::Arc;
use core::{convert::TryFrom as _, fmt, future::Future, pin::Pin};
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};

/// Description of an externality.
pub struct Externality {
    // Called by `start_call`.
    call: fn(
        Box<dyn InternalInterface + Send + Sync>,
        &[vm::RuntimeValue],
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<vm::RuntimeValue>, Error>> + Send + Sync>,
    >,

    /// Name of the function. Used for debugging purposes.
    name: &'static str,
}

trait InternalInterface {
    fn read_memory(
        &self,
        pointer: u32,
        size: u32,
    ) -> Pin<Box<dyn Future<Output = Vec<u8>> + Send + Sync>>;
    fn write_memory(
        &self,
        pointer: u32,
        data: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + Sync>>;
    fn allocate(&self, size: u32) -> Pin<Box<dyn Future<Output = u32> + Send + Sync>>;
    fn free(&self, pointer: u32) -> Pin<Box<dyn Future<Output = ()> + Send + Sync>>;
}

impl InternalInterface for mpsc::Sender<CallStateInner> {
    fn read_memory(
        &self,
        pointer: u32,
        size: u32,
    ) -> Pin<Box<dyn Future<Output = Vec<u8>> + Send + Sync>> {
        let mut sender = self.clone();
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            sender
                .send(CallStateInner::MemoryRead {
                    offset: pointer,
                    size,
                    result: Some(tx),
                })
                .await
                .unwrap();
            rx.await.unwrap()
        })
    }

    fn write_memory(
        &self,
        pointer: u32,
        data: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + Sync>> {
        let mut sender = self.clone();
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            sender
                .send(CallStateInner::MemoryWrite {
                    offset: pointer,
                    data,
                    result: Some(tx),
                })
                .await
                .unwrap();
            rx.await.unwrap()
        })
    }

    fn allocate(&self, size: u32) -> Pin<Box<dyn Future<Output = u32> + Send + Sync>> {
        let mut sender = self.clone();
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            sender
                .send(CallStateInner::Allocation {
                    size,
                    result: Some(tx),
                })
                .await
                .unwrap();
            rx.await.unwrap()
        })
    }

    fn free(&self, pointer: u32) -> Pin<Box<dyn Future<Output = ()> + Send + Sync>> {
        let mut sender = self.clone();
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            sender
                .send(CallStateInner::Dealloc {
                    pointer,
                    result: Some(tx),
                })
                .await
                .unwrap();
            rx.await.unwrap()
        })
    }
}

impl Externality {
    /// Initialize a [`CallState`] that tracks a call to this externality.
    ///
    /// Must pass the parameters of the call.
    // TODO: better param type
    pub fn start_call(&self, params: &[vm::RuntimeValue]) -> CallState {
        let (tx, rx) = mpsc::channel(3);

        CallState {
            future: (self.call)(Box::new(tx), params).fuse(),
            state: CallStateInner::Initial,
            next_state: rx,
        }
    }
}

impl fmt::Debug for Externality {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Externality").field(&self.name).finish()
    }
}

pub struct CallState {
    future: future::Fuse<
        Pin<Box<dyn Future<Output = Result<Option<vm::RuntimeValue>, Error>> + Send + Sync>>,
    >,
    next_state: mpsc::Receiver<CallStateInner>,
    state: CallStateInner,
}

/// Actual implementation of [`CallState`].
enum CallStateInner {
    Initial,
    Finished {
        return_value: Option<vm::RuntimeValue>,
    },
    Error(Error),
    MemoryRead {
        offset: u32,
        size: u32,
        result: Option<oneshot::Sender<Vec<u8>>>,
    },
    MemoryWrite {
        offset: u32,
        data: Vec<u8>,
        result: Option<oneshot::Sender<()>>,
    },
    Allocation {
        size: u32,
        result: Option<oneshot::Sender<u32>>,
    },
    Dealloc {
        pointer: u32,
        result: Option<oneshot::Sender<()>>,
    },
}

impl CallState {
    /// Progresses the call (if possible) and returns the state of the call afterwards. Calling
    /// this function multiple times in a row will always return the same [`State`], unless you
    /// use the [`Resume`] provided in some variants of [`State`] to make the state progress.
    pub fn run(&mut self) -> State {
        // TODO: does this work properly if we call `run` again after the future is finished?
        match (&mut self.future).now_or_never() {
            Some(Ok(val)) => self.state = CallStateInner::Finished { return_value: val },
            Some(Err(err)) => self.state = CallStateInner::Error(err),
            None => {
                if let Some(next) = self.next_state.next().now_or_never() {
                    self.state = next.unwrap();
                }
            }
        }

        match &mut self.state {
            CallStateInner::Finished { return_value } => State::Finished {
                return_value: *return_value,
            },
            CallStateInner::Error(err) => State::Error(err.clone()),
            CallStateInner::MemoryRead {
                offset,
                size,
                result,
            } => State::MemoryReadNeeded {
                offset: *offset,
                size: *size,
                inject_value: Resume { sender: result },
            },
            CallStateInner::MemoryWrite {
                offset,
                data,
                result,
            } => State::MemoryWriteNeeded {
                offset: *offset,
                data,
                done: Resume { sender: result },
            },
            CallStateInner::Initial => unreachable!(),
            CallStateInner::Allocation { size, result } => State::AllocationNeeded {
                size: *size,
                inject_value: Resume { sender: result },
            },
            CallStateInner::Dealloc { pointer, result } => State::UntrustedDealloc {
                pointer: *pointer,
                done: Resume { sender: result },
            },
        }
    }
}

/// Current state of a [`CallState`].
#[derive(Debug)]
pub enum State<'a> {
    /// The call is finished.
    Finished {
        /// Value that the externality must return.
        return_value: Option<vm::RuntimeValue>,
    },

    /// A problem happened during the call.
    Error(Error),

    /// In order to progress, the [`CallState`] needs to know the content of the memory at the
    /// given location.
    // TODO: could have a more zero-cost API by not requiring a Vec allocation, but let's first
    // implement everything in order to see when exactly do we need memory reads
    MemoryReadNeeded {
        /// Offset in memory where to read.
        offset: u32,
        /// Size to read.
        size: u32,
        /// Object to use to inject the memory content and update the state.
        inject_value: Resume<'a, Vec<u8>>,
    },

    /// The [`CallState`] signals that memory needs to be written at the given location.
    MemoryWriteNeeded {
        /// Offset in memory where to write.
        offset: u32,
        /// Data to write.
        data: &'a [u8],
        /// Object to signal that this is done.
        done: Resume<'a, ()>,
    },

    /// Request to allocate some memory in the Wasm virtual memory.
    AllocationNeeded {
        /// Requested size, in bytes, for the allocation.
        size: u32,
        /// Object to use to inject the allocated pointer and update the state.
        inject_value: Resume<'a, u32>,
    },

    /// Free the memory allocated by the given pointer. The pointer might not necessarily be
    /// valid. This can't cause any unsafety for the host.
    UntrustedDealloc {
        /// Pointer that was previously returned by an allocation request.
        pointer: u32,
        /// Object to signal that this is done.
        done: Resume<'a, ()>,
    },
}

/// Object that allows injecting the result of an operation into the [`CallState`].
pub struct Resume<'a, T> {
    sender: &'a mut Option<oneshot::Sender<T>>,
}

impl<'a, T> Resume<'a, T> {
    /// Injects the value.
    pub fn inject(self, value: T) {
        match self.sender.take().unwrap().send(value) {
            Ok(()) => {}
            Err(_) => unreachable!(),
        }
    }
}

impl<'a, T> fmt::Debug for Resume<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Resume").finish()
    }
}

#[derive(Debug, Clone, derive_more::Display)]
pub enum Error {
    /// Mismatch between the number of parameters expected and the actual number.
    ParamsCountMismatch,
    /// The type of one of the parameters is wrong.
    WrongParamTy,
}

/// Returns the definition of a function by its name. Returns `None` if the function is unknown.
// TODO: should make it possible to filter categories of functions, so that offchain worker
// functions are for example forbidden for regular blocks
pub(super) fn function_by_name(name: &str) -> Option<Externality> {
    match name {
        /*"ext_storage_set_version_1" => Externality::Interrupt {
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
        "ext_storage_get_version_1" => Externality::Interrupt {
            implementation: Box::new(|params, vm, _alloc| {
                let storage_key = expect_one_ptr_len(&params, vm);
                StateInner::ExternalStorageGet { storage_key }
            }),
        },
        "ext_storage_read_version_1" => unimplemented!(),
        "ext_storage_clear_version_1" => Externality::Interrupt {
            implementation: Box::new(|params, vm, _alloc| {
                let storage_key = expect_one_ptr_len(&params, vm);
                StateInner::ExternalStorageClear { storage_key }
            }),
        },
        "ext_storage_exists_version_1" => unimplemented!(),
        "ext_storage_clear_prefix_version_1" => unimplemented!(),
        "ext_storage_root_version_1" => unimplemented!(),
        "ext_storage_changes_root_version_1" => unimplemented!(),
        "ext_storage_next_key_version_1" => unimplemented!(),
        "ext_storage_child_set_version_1" => unimplemented!(),
        "ext_storage_child_get_version_1" => unimplemented!(),
        "ext_storage_child_read_version_1" => unimplemented!(),
        "ext_storage_child_clear_version_1" => unimplemented!(),
        "ext_storage_child_storage_kill_version_1" => unimplemented!(),
        "ext_storage_child_exists_version_1" => unimplemented!(),
        "ext_storage_child_clear_prefix_version_1" => unimplemented!(),
        "ext_storage_child_root_version_1" => unimplemented!(),
        "ext_storage_child_next_key_version_1" => unimplemented!(),

        "ext_crypto_ed25519_public_keys_version_1" => unimplemented!(),
        "ext_crypto_ed25519_generate_version_1" => unimplemented!(),
        "ext_crypto_ed25519_sign_version_1" => unimplemented!(),
        "ext_crypto_ed25519_verify_version_1" => unimplemented!(),
        "ext_crypto_sr25519_public_keys_version_1" => unimplemented!(),
        "ext_crypto_sr25519_generate_version_1" => unimplemented!(),
        "ext_crypto_sr25519_sign_version_1" => unimplemented!(),
        "ext_crypto_sr25519_verify_version_1" => unimplemented!(),
        "ext_crypto_secp256k1_ecdsa_recover_version_1" => unimplemented!(),
        "ext_crypto_secp256k1_ecdsa_recover_compressed_version_1" => {
            Externality::Immediate {
                implementation: Box::new(|_, _, _| unimplemented!()),
            }
        }

        "ext_hashing_keccak_256_version_1" => unimplemented!(),
        "ext_hashing_sha2_256_version_1" => unimplemented!(),
        "ext_hashing_blake2_128_version_1" => unimplemented!(),
        "ext_hashing_blake2_256_version_1" => unimplemented!(),
        "ext_hashing_twox_64_version_1" => Externality::Immediate {
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
        "ext_hashing_twox_128_version_1" => Externality::Immediate {
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
        "ext_hashing_twox_256_version_1" => Externality::Immediate {
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

        "ext_offchain_is_validator_version_1" => unimplemented!(),
        "ext_offchain_submit_transaction_version_1" => unimplemented!(),
        "ext_offchain_network_state_version_1" => unimplemented!(),
        "ext_offchain_timestamp_version_1" => unimplemented!(),
        "ext_offchain_sleep_until_version_1" => unimplemented!(),
        "ext_offchain_random_seed_version_1" => unimplemented!(),
        "ext_offchain_local_storage_set_version_1" => unimplemented!(),
        "ext_offchain_local_storage_compare_and_set_version_1" => unimplemented!(),
        "ext_offchain_local_storage_get_version_1" => unimplemented!(),
        "ext_offchain_http_request_start_version_1" => unimplemented!(),
        "ext_offchain_http_request_add_header_version_1" => unimplemented!(),
        "ext_offchain_http_request_write_body_version_1" => unimplemented!(),
        "ext_offchain_http_response_wait_version_1" => unimplemented!(),
        "ext_offchain_http_response_headers_version_1" => unimplemented!(),
        "ext_offchain_http_response_read_body_version_1" => unimplemented!(),

        "ext_trie_blake2_256_root_version_1" => unimplemented!(),
        "ext_trie_blake2_256_ordered_root_version_1" => unimplemented!(),

        "ext_misc_chain_id_version_1" => unimplemented!(),
        "ext_misc_print_num_version_1" => unimplemented!(),
        "ext_misc_print_utf8_version_1" => unimplemented!(),
        "ext_misc_print_hex_version_1" => unimplemented!(),
        "ext_misc_runtime_version_version_1" => unimplemented!(),*/

        "ext_allocator_malloc_version_1" => Some(Externality {
            name: "ext_allocator_malloc_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let size = expect_u32(&params[0])?;
                    let ptr = interface.allocate(size).await;
                    let ptr_i32 = i32::from_ne_bytes(ptr.to_ne_bytes());
                    Ok(Some(vm::RuntimeValue::I32(ptr_i32)))
                })
            },
        }),
        "ext_allocator_free_version_1" => Some(Externality {
            name: "ext_allocator_free_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let pointer = expect_u32(&params[0])?;
                    interface.free(pointer).await;
                    Ok(None)
                })
            },
        }),

        "ext_logging_log_version_1" => Some(Externality {
            name: "ext_logging_log_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(3, &params)?;
                    let log_level = expect_u32(&params[0])?;
                    let target = expect_pointer_size(&params[1], &*interface).await;
                    let message = expect_pointer_size(&params[2], &*interface).await;

                    // TODO: properly print message
                    println!("runtime log: {:?} {:?}", target, message);

                    Ok(None)
                })
            },
        }),

        // All unknown functions.
        _ => None,
    }
}

/// Utility function that returns an error if `params.len()` is not equal to `expected_num`.
fn expect_num_params(expected_num: usize, params: &[vm::RuntimeValue]) -> Result<(), Error> {
    if expected_num == params.len() {
        Ok(())
    } else {
        Err(Error::ParamsCountMismatch)
    }
}

/// Utility function that turns a parameter into a `u32`, or returns an error if it is impossible.
fn expect_u32(param: &vm::RuntimeValue) -> Result<u32, Error> {
    match param {
        vm::RuntimeValue::I32(v) => Ok(u32::from_ne_bytes(v.to_ne_bytes())),
        _ => Err(Error::WrongParamTy),
    }
}

/// Utility function that turns a "pointer-size" parameter into the memory content, or returns an
/// error if it is impossible.
async fn expect_pointer_size(
    param: &vm::RuntimeValue,
    interface: &(dyn InternalInterface + Send + Sync),
) -> Result<Vec<u8>, Error> {
    let val = match param {
        vm::RuntimeValue::I64(v) => u64::from_ne_bytes(v.to_ne_bytes()),
        _ => return Err(Error::WrongParamTy),
    };

    let len = u32::try_from(val >> 32).unwrap();
    let ptr = u32::try_from(val & 0xffffffff).unwrap();

    Ok(interface.read_memory(ptr, len).await)
}

#[cfg(test)]
mod tests {
    use super::{function_by_name, CallState, Externality, Resume, State};

    #[test]
    fn usage_example() {
        let function = function_by_name("ext_hashing_sha2_256_version_1").unwrap();
        let call_state = function.start_call(&[]);
        // TODO: finish writing this test
    }
}
