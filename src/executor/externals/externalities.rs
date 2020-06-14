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

use core::{convert::TryFrom as _, fmt, future::Future, hash::Hasher as _, pin::Pin};
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use parity_scale_codec::DecodeAll as _;

use tiny_keccak::Hasher as _;

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

macro_rules! define_internal_interface {
    ($($variant:ident => fn $name:ident($($param_name:ident: $param_ty:ty),*) -> $ret:ty;)*) => {
        trait InternalInterface {
            $(
                fn $name(&self, $($param_name: $param_ty),*) -> Pin<Box<dyn Future<Output = $ret> + Send + Sync>>;
            )*
        }

        impl InternalInterface for mpsc::Sender<CallStateInner> {
            $(
                fn $name(&self, $($param_name: $param_ty),*) -> Pin<Box<dyn Future<Output = $ret> + Send + Sync>> {
                    let mut sender = self.clone();
                    Box::pin(async move {
                        let (tx, rx) = oneshot::channel();
                        sender
                            .send(CallStateInner::$variant {
                                $($param_name,)*
                                result: Some(tx),
                            })
                            .await
                            .unwrap();
                        rx.await.unwrap()
                    })
                }
            )*
        }

        /// Actual implementation of [`CallState`].
        #[derive(Debug)]
        enum CallStateInner {
            Initial,
            Finished {
                return_value: Option<vm::RuntimeValue>,
            },
            Error(Error),
            $($variant {
                $($param_name: $param_ty,)*
                result: Option<oneshot::Sender<$ret>>,
            },)*
        }
    }
}

define_internal_interface! {
    MemoryRead => fn read_memory(offset: u32, size: u32) -> Vec<u8>;
    MemoryWrite => fn write_memory(offset: u32, data: Vec<u8>) -> ();
    Allocation => fn allocate(size: u32) -> u32;
    Dealloc => fn free(pointer: u32) -> ();
    StorageSet => fn storage_set(key: Vec<u8>, value: Option<Vec<u8>>) -> ();
    StorageAppend => fn storage_append(key: Vec<u8>, value: Vec<u8>) -> ();
    StorageGet => fn storage_get(key: Vec<u8>, offset: u32, max_size: u32) -> Option<Vec<u8>>;
    StorageClearPrefix => fn storage_clear_prefix(key: Vec<u8>) -> ();
    StorageRoot => fn storage_root() -> [u8; 32];
    StorageChangesRoot => fn storage_changes_root(parent_hash: Vec<u8>) -> Option<[u8; 32]>;
    StorageNextKey => fn storage_next_key(key: Vec<u8>) -> Option<Vec<u8>>;
}

impl Externality {
    /// Initialize a [`CallState`] that tracks a call to this externality.
    ///
    /// Must pass the parameters of the call.
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

impl CallState {
    /// Progresses the call (if possible) and returns the state of the call afterwards. Calling
    /// this function multiple times in a row will always return the same [`State`], unless you
    /// use the [`Resume`] provided in some variants of [`State`] to make the state progress.
    pub fn run(&mut self) -> State {
        // TODO: make sure this work properly if we call `run` again after the future is finished
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
            CallStateInner::StorageGet {
                key,
                offset,
                max_size,
                result,
            } => State::StorageGetNeeded {
                key: key,
                offset: *offset,
                max_size: *max_size,
                done: Resume { sender: result },
            },
            CallStateInner::StorageSet { key, value, result } => State::StorageSetNeeded {
                key,
                value: value.as_ref().map(|v| &**v),
                done: Resume { sender: result },
            },
            CallStateInner::StorageAppend { key, value, result } => State::StorageAppendNeeded {
                key,
                value: &**value,
                done: Resume { sender: result },
            },
            CallStateInner::StorageClearPrefix { key, result } => State::StorageClearPrefixNeeded {
                key,
                done: Resume { sender: result },
            },
            CallStateInner::StorageRoot { result } => State::StorageRootNeeded {
                done: Resume { sender: result },
            },
            CallStateInner::StorageChangesRoot {
                parent_hash,
                result,
            } => State::StorageChangesRootNeeded {
                parent_hash: &**parent_hash,
                done: Resume { sender: result },
            },
            CallStateInner::StorageNextKey { key, result } => State::StorageNextKeyNeeded {
                key,
                done: Resume { sender: result },
            },
            st => unimplemented!("unimplemented state: {:?}", st), // TODO:
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

    StorageGetNeeded {
        key: &'a [u8],
        offset: u32,
        max_size: u32,
        done: Resume<'a, Option<Vec<u8>>>,
    },
    StorageSetNeeded {
        key: &'a [u8],
        value: Option<&'a [u8]>,
        done: Resume<'a, ()>,
    },
    StorageAppendNeeded {
        key: &'a [u8],
        value: &'a [u8],
        done: Resume<'a, ()>,
    },
    StorageClearPrefixNeeded {
        key: &'a [u8],
        done: Resume<'a, ()>,
    },
    StorageRootNeeded {
        done: Resume<'a, [u8; 32]>,
    },
    StorageChangesRootNeeded {
        parent_hash: &'a [u8],
        done: Resume<'a, Option<[u8; 32]>>,
    },
    StorageNextKeyNeeded {
        key: &'a [u8],
        done: Resume<'a, Option<Vec<u8>>>,
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

#[derive(Debug, Clone, derive_more::Display, derive_more::From)]
pub enum Error {
    /// Mismatch between the number of parameters expected and the actual number.
    ParamsCountMismatch,
    /// Failed to decode a SCALE-encoded parameter.
    ParamDecodeError(parity_scale_codec::Error),
    /// The type of one of the parameters is wrong.
    WrongParamTy,
}

/// Returns the definition of a function by its name. Returns `None` if the function is unknown.
// TODO: should make it possible to filter categories of functions, so that offchain worker
// functions are for example forbidden for regular blocks
// TODO: are all these functions still relevant? have some been removed? the specs are quite old
// in this regard
pub(super) fn function_by_name(name: &str) -> Option<Externality> {
    match name {
        "ext_storage_set_version_1" => Some(Externality {
            name: "ext_storage_set_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(2, &params)?;
                    let key = expect_pointer_size(&params[0], &*interface).await?;
                    let value = expect_pointer_size(&params[1], &*interface).await?;
                    interface.storage_set(key, Some(value)).await;
                    Ok(None)
                })
            },
        }),
        "ext_storage_get_version_1" => Some(Externality {
            name: "ext_storage_get_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let key = expect_pointer_size(&params[0], &*interface).await?;
                    let value = interface.storage_get(key, 0, u32::max_value()).await;
                    // TODO: we SCALE-encode an option, meaning we just memcpy the value, this is
                    // extremely stupid
                    let value_encoded = parity_scale_codec::Encode::encode(&value);
                    // TODO: don't unwrap?
                    let value_len = u32::try_from(value_encoded.len()).unwrap();

                    let dest_ptr = interface.allocate(value_len).await;
                    interface.write_memory(dest_ptr, value_encoded).await;

                    let ret = build_pointer_size(dest_ptr, value_len);
                    Ok(Some(vm::RuntimeValue::I64(reinterpret_u64_i64(ret))))
                })
            },
        }),
        "ext_storage_read_version_1" => Some(Externality {
            name: "ext_storage_read_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(3, &params)?;
                    let key = expect_pointer_size(&params[0], &*interface).await?;
                    let (value_out_ptr, value_out_size) = expect_pointer_size_raw(&params[1])?;
                    let offset = expect_u32(&params[2])?;

                    let value = interface.storage_get(key, offset, value_out_size).await;
                    let outcome = if let Some(value) = value {
                        let written = u32::try_from(value.len()).unwrap();
                        assert!(written <= value_out_size);
                        interface.write_memory(value_out_ptr, value).await;
                        Some(written)
                    } else {
                        None
                    };

                    let outcome_encoded = parity_scale_codec::Encode::encode(&outcome);
                    let outcome_encoded_len = u32::try_from(outcome_encoded.len()).unwrap();
                    let dest_ptr = interface.allocate(outcome_encoded_len).await;
                    interface.write_memory(dest_ptr, outcome_encoded).await;

                    let ret = build_pointer_size(dest_ptr, outcome_encoded_len);
                    Ok(Some(vm::RuntimeValue::I64(reinterpret_u64_i64(ret))))
                })
            },
        }),
        "ext_storage_clear_version_1" => Some(Externality {
            name: "ext_storage_clear_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let key = expect_pointer_size(&params[0], &*interface).await?;
                    interface.storage_set(key, None).await;
                    Ok(None)
                })
            },
        }),
        "ext_storage_exists_version_1" => Some(Externality {
            name: "ext_storage_exists_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let key = expect_pointer_size(&params[0], &*interface).await?;
                    let value = interface.storage_get(key, 0, 0).await;
                    if value.is_some() {
                        Ok(Some(vm::RuntimeValue::I32(1)))
                    } else {
                        Ok(Some(vm::RuntimeValue::I32(0)))
                    }
                })
            },
        }),
        "ext_storage_clear_prefix_version_1" => Some(Externality {
            name: "ext_storage_clear_prefix_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let key = expect_pointer_size(&params[0], &*interface).await?;
                    interface.storage_clear_prefix(key).await;
                    Ok(None)
                })
            },
        }),
        "ext_storage_root_version_1" => Some(Externality {
            name: "ext_storage_root_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(0, &params)?;
                    let hash = interface.storage_root().await;
                    // TODO: that's some next-level inefficiency here
                    let encoded_hash = parity_scale_codec::Encode::encode(&hash);
                    let value_len = u32::try_from(encoded_hash.len()).unwrap();

                    let dest_ptr = interface.allocate(value_len).await;
                    interface.write_memory(dest_ptr, encoded_hash).await;

                    let ret = build_pointer_size(dest_ptr, value_len);
                    Ok(Some(vm::RuntimeValue::I64(reinterpret_u64_i64(ret))))
                })
            },
        }),
        "ext_storage_changes_root_version_1" => Some(Externality {
            name: "ext_storage_changes_root_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let parent_hash = expect_pointer_size(&params[0], &*interface).await?;
                    let hash = interface.storage_changes_root(parent_hash).await;

                    // TODO: that's some next-level inefficiency here
                    let encoded_hash = parity_scale_codec::Encode::encode(&hash);
                    let value_len = u32::try_from(encoded_hash.len()).unwrap();

                    let dest_ptr = interface.allocate(value_len).await;
                    interface.write_memory(dest_ptr, encoded_hash).await;

                    let ret = build_pointer_size(dest_ptr, value_len);
                    Ok(Some(vm::RuntimeValue::I64(reinterpret_u64_i64(ret))))
                })
            },
        }),
        "ext_storage_next_key_version_1" => Some(Externality {
            name: "ext_storage_next_key_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let key = expect_pointer_size(&params[0], &*interface).await?;
                    let value = interface.storage_next_key(key).await;
                    // TODO: we SCALE-encode an option, meaning we just memcpy the value, this is
                    // extremely stupid
                    let value_encoded = parity_scale_codec::Encode::encode(&value);
                    // TODO: don't unwrap?
                    let value_len = u32::try_from(value_encoded.len()).unwrap();

                    let dest_ptr = interface.allocate(value_len).await;
                    interface.write_memory(dest_ptr, value_encoded).await;

                    let ret = build_pointer_size(dest_ptr, value_len);
                    Ok(Some(vm::RuntimeValue::I64(reinterpret_u64_i64(ret))))
                })
            },
        }),
        "ext_storage_append_version_1" => Some(Externality {
            name: "ext_storage_append_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(2, &params)?;
                    let key = expect_pointer_size(&params[0], &*interface).await?;
                    let value = expect_pointer_size(&params[1], &*interface).await?;
                    interface.storage_append(key, value).await;
                    Ok(None)
                })
            },
        }),
        "ext_storage_child_set_version_1" => Some(Externality {
            name: "ext_storage_child_set_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_storage_child_get_version_1" => Some(Externality {
            name: "ext_storage_child_get_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_storage_child_read_version_1" => Some(Externality {
            name: "ext_storage_child_read_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_storage_child_clear_version_1" => Some(Externality {
            name: "ext_storage_child_clear_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_storage_child_storage_kill_version_1" => Some(Externality {
            name: "ext_storage_child_storage_kill_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_storage_child_exists_version_1" => Some(Externality {
            name: "ext_storage_child_exists_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_storage_child_clear_prefix_version_1" => Some(Externality {
            name: "ext_storage_child_clear_prefix_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_storage_child_root_version_1" => Some(Externality {
            name: "ext_storage_child_root_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_storage_child_next_key_version_1" => Some(Externality {
            name: "ext_storage_child_next_key_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_default_child_storage_get_version_1" => Some(Externality {
            name: "ext_default_child_storage_get_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_default_child_storage_storage_kill_version_1" => Some(Externality {
            name: "ext_default_child_storage_storage_kill_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_default_child_storage_set_version_1" => Some(Externality {
            name: "ext_default_child_storage_set_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_default_child_storage_clear_version_1" => Some(Externality {
            name: "ext_default_child_storage_clear_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_default_child_storage_root_version_1" => Some(Externality {
            name: "ext_default_child_storage_root_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),

        "ext_crypto_ed25519_public_keys_version_1" => Some(Externality {
            name: "ext_crypto_ed25519_public_keys_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_crypto_ed25519_generate_version_1" => Some(Externality {
            name: "ext_crypto_ed25519_generate_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_crypto_ed25519_sign_version_1" => Some(Externality {
            name: "ext_crypto_ed25519_sign_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_crypto_ed25519_verify_version_1" => Some(Externality {
            name: "ext_crypto_ed25519_verify_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_crypto_sr25519_public_keys_version_1" => Some(Externality {
            name: "ext_crypto_sr25519_public_keys_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_crypto_sr25519_generate_version_1" => Some(Externality {
            name: "ext_crypto_sr25519_generate_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_crypto_sr25519_sign_version_1" => Some(Externality {
            name: "ext_crypto_sr25519_sign_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_crypto_sr25519_verify_version_1" => Some(Externality {
            name: "ext_crypto_sr25519_verify_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_crypto_sr25519_verify_version_2" => Some(Externality {
            name: "ext_crypto_sr25519_verify_version_2",
            call: |_interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(3, &params)?;
                    // TODO: wrong! this is a dummy implementation meaning that all signature
                    // verifications are always successful
                    Ok(Some(vm::RuntimeValue::I32(1)))
                })
            },
        }),
        "ext_crypto_secp256k1_ecdsa_recover_version_1" => Some(Externality {
            name: "ext_crypto_secp256k1_ecdsa_recover_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    #[derive(parity_scale_codec::Encode)]
                    enum EcdsaVerifyError {
                        BadRS,
                        BadV,
                        BadSignature,
                    }

                    expect_num_params(2, &params)?;
                    let sig = expect_constant_size_pointer(&params[0], 65, &*interface).await?;
                    let msg = expect_constant_size_pointer(&params[1], 32, &*interface).await?;

                    let result = (|| -> Result<_, EcdsaVerifyError> {
                        let rs = secp256k1::Signature::parse_slice(&sig[0..64])
                            .map_err(|_| EcdsaVerifyError::BadRS)?;
                        let v = secp256k1::RecoveryId::parse(if sig[64] > 26 {
                            sig[64] - 27
                        } else {
                            sig[64]
                        } as u8)
                        .map_err(|_| EcdsaVerifyError::BadV)?;
                        let pubkey = secp256k1::recover(
                            &secp256k1::Message::parse_slice(&msg).unwrap(),
                            &rs,
                            &v,
                        )
                        .map_err(|_| EcdsaVerifyError::BadSignature)?;
                        let mut res = [0u8; 64];
                        res.copy_from_slice(&pubkey.serialize()[1..65]);
                        Ok(res)
                    })();
                    let result_encoded = parity_scale_codec::Encode::encode(&result);
                    let result_encoded_len = u32::try_from(result_encoded.len()).unwrap();

                    let dest_ptr = interface.allocate(result_encoded_len).await;
                    interface.write_memory(dest_ptr, result_encoded).await;

                    let ret = build_pointer_size(dest_ptr, result_encoded_len);
                    Ok(Some(vm::RuntimeValue::I64(reinterpret_u64_i64(ret))))
                })
            },
        }),
        "ext_crypto_secp256k1_ecdsa_recover_compressed_version_1" => Some(Externality {
            name: "ext_crypto_secp256k1_ecdsa_recover_compressed_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_crypto_start_batch_verify_version_1" => Some(Externality {
            name: "ext_crypto_start_batch_verify_version_1",
            call: |_interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(0, &params)?;
                    Ok(None)
                })
            },
        }),
        "ext_crypto_finish_batch_verify_version_1" => Some(Externality {
            name: "ext_crypto_finish_batch_verify_version_1",
            call: |_interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(0, &params)?;
                    // TODO: wrong! this is a dummy implementation meaning that all signature
                    // verifications are always successful
                    Ok(Some(vm::RuntimeValue::I32(1)))
                })
            },
        }),

        "ext_hashing_keccak_256_version_1" => Some(Externality {
            name: "ext_hashing_keccak_256_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let data = expect_pointer_size(&params[0], &*interface).await?;

                    let mut keccak = tiny_keccak::Keccak::v256();
                    keccak.update(&data);
                    let mut out = [0u8; 32];
                    keccak.finalize(&mut out);

                    let dest_ptr = interface.allocate(32).await;
                    interface.write_memory(dest_ptr, out.to_vec()).await;
                    Ok(Some(vm::RuntimeValue::I32(reinterpret_u32_i32(dest_ptr))))
                })
            },
        }),
        "ext_hashing_sha2_256_version_1" => Some(Externality {
            name: "ext_hashing_sha2_256_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_hashing_blake2_128_version_1" => Some(Externality {
            name: "ext_hashing_blake2_128_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let data = expect_pointer_size(&params[0], &*interface).await?;

                    let out = blake2_rfc::blake2b::blake2b(16, &[], &data);

                    let dest_ptr = interface.allocate(16).await;
                    interface
                        .write_memory(dest_ptr, out.as_bytes().to_vec())
                        .await;
                    Ok(Some(vm::RuntimeValue::I32(reinterpret_u32_i32(dest_ptr))))
                })
            },
        }),
        "ext_hashing_blake2_256_version_1" => Some(Externality {
            name: "ext_hashing_blake2_256_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let data = expect_pointer_size(&params[0], &*interface).await?;

                    let out = blake2_rfc::blake2b::blake2b(32, &[], &data);

                    let dest_ptr = interface.allocate(32).await;
                    interface
                        .write_memory(dest_ptr, out.as_bytes().to_vec())
                        .await;
                    Ok(Some(vm::RuntimeValue::I32(reinterpret_u32_i32(dest_ptr))))
                })
            },
        }),
        "ext_hashing_twox_64_version_1" => Some(Externality {
            name: "ext_hashing_twox_64_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let data = expect_pointer_size(&params[0], &*interface).await?;

                    let mut h0 = twox_hash::XxHash::with_seed(0);
                    h0.write(&data);
                    let r0 = h0.finish();

                    let dest_ptr = interface.allocate(8).await;
                    interface
                        .write_memory(dest_ptr, r0.to_le_bytes().to_vec())
                        .await;
                    Ok(Some(vm::RuntimeValue::I32(reinterpret_u32_i32(dest_ptr))))
                })
            },
        }),
        "ext_hashing_twox_128_version_1" => Some(Externality {
            name: "ext_hashing_twox_128_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let data = expect_pointer_size(&params[0], &*interface).await?;

                    let mut h0 = twox_hash::XxHash::with_seed(0);
                    let mut h1 = twox_hash::XxHash::with_seed(1);
                    h0.write(&data);
                    h1.write(&data);
                    let r0 = h0.finish();
                    let r1 = h1.finish();

                    let mut out_value = r0.to_le_bytes().to_vec();
                    out_value.extend(&r1.to_le_bytes());

                    let dest_ptr = interface.allocate(16).await;
                    interface.write_memory(dest_ptr, out_value).await;
                    Ok(Some(vm::RuntimeValue::I32(reinterpret_u32_i32(dest_ptr))))
                })
            },
        }),
        "ext_hashing_twox_256_version_1" => Some(Externality {
            name: "ext_hashing_twox_256_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let data = expect_pointer_size(&params[0], &*interface).await?;

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

                    let mut out_value = r0.to_le_bytes().to_vec();
                    out_value.extend(&r1.to_le_bytes());
                    out_value.extend(&r2.to_le_bytes());
                    out_value.extend(&r3.to_le_bytes());

                    let dest_ptr = interface.allocate(32).await;
                    interface.write_memory(dest_ptr, out_value).await;
                    Ok(Some(vm::RuntimeValue::I32(reinterpret_u32_i32(dest_ptr))))
                })
            },
        }),

        "ext_offchain_is_validator_version_1" => Some(Externality {
            name: "ext_offchain_is_validator_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_submit_transaction_version_1" => Some(Externality {
            name: "ext_offchain_submit_transaction_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_network_state_version_1" => Some(Externality {
            name: "ext_offchain_network_state_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_timestamp_version_1" => Some(Externality {
            name: "ext_offchain_timestamp_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_sleep_until_version_1" => Some(Externality {
            name: "ext_offchain_sleep_until_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_random_seed_version_1" => Some(Externality {
            name: "ext_offchain_random_seed_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_local_storage_set_version_1" => Some(Externality {
            name: "ext_offchain_local_storage_set_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_local_storage_compare_and_set_version_1" => Some(Externality {
            name: "ext_offchain_local_storage_compare_and_set_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_local_storage_get_version_1" => Some(Externality {
            name: "ext_offchain_local_storage_get_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_http_request_start_version_1" => Some(Externality {
            name: "ext_offchain_http_request_start_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_http_request_add_header_version_1" => Some(Externality {
            name: "ext_offchain_http_request_add_header_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_http_request_write_body_version_1" => Some(Externality {
            name: "ext_offchain_http_request_write_body_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_http_response_wait_version_1" => Some(Externality {
            name: "ext_offchain_http_response_wait_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_http_response_headers_version_1" => Some(Externality {
            name: "ext_offchain_http_response_headers_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_offchain_http_response_read_body_version_1" => Some(Externality {
            name: "ext_offchain_http_response_read_body_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),

        "ext_trie_blake2_256_root_version_1" => Some(Externality {
            name: "ext_trie_blake2_256_root_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let encoded = expect_pointer_size(&params[0], &*interface).await?;
                    let elements = Vec::<(Vec<u8>, Vec<u8>)>::decode_all(&encoded)?;

                    let mut trie = crate::trie::Trie::new();
                    for (key, value) in elements {
                        trie.insert(&key, value);
                    }
                    let out = trie.root_merkle_value(None);

                    let dest_ptr = interface.allocate(32).await;
                    interface.write_memory(dest_ptr, out.to_vec()).await;
                    Ok(Some(vm::RuntimeValue::I32(reinterpret_u32_i32(dest_ptr))))
                })
            },
        }),
        "ext_trie_blake2_256_ordered_root_version_1" => Some(Externality {
            name: "ext_trie_blake2_256_ordered_root_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let encoded = expect_pointer_size(&params[0], &*interface).await?;
                    let elements = Vec::<Vec<u8>>::decode_all(&encoded)?;

                    let mut trie = crate::trie::Trie::new();
                    for (idx, value) in elements.into_iter().enumerate() {
                        let idx = u32::try_from(idx).unwrap();
                        let key =
                            parity_scale_codec::Encode::encode(&parity_scale_codec::Compact(idx));
                        trie.insert(&key, value);
                    }
                    let out = trie.root_merkle_value(None);

                    let dest_ptr = interface.allocate(32).await;
                    interface.write_memory(dest_ptr, out.to_vec()).await;
                    Ok(Some(vm::RuntimeValue::I32(reinterpret_u32_i32(dest_ptr))))
                })
            },
        }),

        "ext_misc_chain_id_version_1" => Some(Externality {
            name: "ext_misc_chain_id_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_misc_print_num_version_1" => Some(Externality {
            name: "ext_misc_print_num_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
        "ext_misc_print_utf8_version_1" => Some(Externality {
            name: "ext_misc_print_utf8_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let message = expect_pointer_size(&params[0], &*interface).await?;

                    // TODO: properly print message
                    if let Ok(message) = core::str::from_utf8(&message) {
                        println!("print_utf8: {:?}", message);
                    }

                    Ok(None)
                })
            },
        }),
        "ext_misc_print_hex_version_1" => Some(Externality {
            name: "ext_misc_print_hex_version_1",
            call: |interface, params| {
                let params = params.to_vec();
                Box::pin(async move {
                    expect_num_params(1, &params)?;
                    let data = expect_pointer_size(&params[0], &*interface).await?;
                    let message = hex::encode(&data);

                    // TODO: properly print message
                    println!("print_hex: {}", message);
                    Ok(None)
                })
            },
        }),
        "ext_misc_runtime_version_version_1" => Some(Externality {
            name: "ext_misc_runtime_version_version_1",
            call: |_interface, params| {
                let _params = params.to_vec();
                Box::pin(async move { unimplemented!() })
            },
        }),
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
                    let _log_level = expect_u32(&params[0])?;
                    let target = expect_pointer_size(&params[1], &*interface).await?;
                    let message = expect_pointer_size(&params[2], &*interface).await?;

                    // TODO: properly print message
                    if let (Ok(target), Ok(message)) = (
                        core::str::from_utf8(&target),
                        core::str::from_utf8(&message),
                    ) {
                        println!("runtime log: {:?} {:?}", target, message);
                    }

                    Ok(None)
                })
            },
        }),

        // All unknown functions.
        // TODO: since the specs aren't up-to-date with the list of functions, we just resolve
        // everything as valid, and panic if an unknown function is actually called
        //_ => None,
        _f => {
            // TODO: this println is a bit too verbose
            //println!("unknown function: {}", f);
            Some(Externality {
                name: "unknown",
                call: |_, _| unimplemented!(),
            })
        }
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

/// Utility function that turns a parameter into a "pointer-size" parameter, or returns an error
/// if it is impossible.
fn expect_pointer_size_raw(param: &vm::RuntimeValue) -> Result<(u32, u32), Error> {
    let val = match param {
        vm::RuntimeValue::I64(v) => u64::from_ne_bytes(v.to_ne_bytes()),
        _ => return Err(Error::WrongParamTy),
    };

    let len = u32::try_from(val >> 32).unwrap();
    let ptr = u32::try_from(val & 0xffffffff).unwrap();
    Ok((ptr, len))
}

/// Utility function that turns a "pointer-size" parameter into the memory content, or returns an
/// error if it is impossible.
async fn expect_pointer_size(
    param: &vm::RuntimeValue,
    interface: &(dyn InternalInterface + Send + Sync),
) -> Result<Vec<u8>, Error> {
    let (ptr, len) = expect_pointer_size_raw(param)?;
    Ok(interface.read_memory(ptr, len).await)
}

/// Utility function that turns a pointer parameter into the memory content, or returns an error
/// if it is impossible.
async fn expect_constant_size_pointer(
    param: &vm::RuntimeValue,
    memory_size: u32,
    interface: &(dyn InternalInterface + Send + Sync),
) -> Result<Vec<u8>, Error> {
    let ptr = expect_u32(param)?;
    Ok(interface.read_memory(ptr, memory_size).await)
}

/// Builds a "pointer-size" value.
fn build_pointer_size(pointer: u32, size: u32) -> u64 {
    (u64::from(size) << 32) | u64::from(pointer)
}

/// Utility function that "reinterprets" a u32 into a i32. That is, the underlying bits stay the
/// same.
fn reinterpret_u32_i32(val: u32) -> i32 {
    i32::from_ne_bytes(val.to_ne_bytes())
}

/// Utility function that "reinterprets" a u64 into a i64. That is, the underlying bits stay the
/// same.
fn reinterpret_u64_i64(val: u64) -> i64 {
    i64::from_ne_bytes(val.to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use super::function_by_name;

    #[test]
    fn usage_example() {
        let function = function_by_name("ext_hashing_sha2_256_version_1").unwrap();
        let _call_state = function.start_call(&[]);
        // TODO: finish writing this test
    }
}
