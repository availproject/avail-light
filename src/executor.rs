//! Wasm runtime code execution.
//!
//! WebAssembly (often abbreviated *Wasm*) plays a big role in Substrate/Polkadot. The storage of
//! each block in the chain has a special key named `:code` which contains the WebAssembly code
//! of what we call *the runtime*.
//!
//! The runtime is a program (compiled in WebAssembly) that decides, amongst other things, whether
//! transactions are valid and how to apply them on the storage.
//!
//! This module contains everything necessary to execute the runtime.
//!
//! # Usage
//!
//! The first step is to create a [`WasmBlob`] object from the WebAssembly code. Creating this
//! object performs some initial steps, such as parsing the WebAssembly code. You are encouraged
//! to maintain a cache of [`WasmBlob`] objects (one instance per WebAssembly byte code) in order
//! to avoid performing these operations too often.
//!
//! To start calling the runtime, create a [`WasmVm`] object, passing the [`WasmBlob`] and a
//! [`FunctionToCall`].
//!
//! While the Wasm runtime cide has side-effects (such as storing values in the storage), the
//! [`WasmVm`] itself is a pure state machine with no side effects.
//!
//! At any given point, you can call [`WasmVm::state`] in order to know in which state the
//! execution currently is.
//! If [`State::ReadyToRun`] is returned (which initially is the case when you create the
//! [`WasmVm`]), then you can execute the Wasm code by calling [`ReadyToRun::run`].
//! No background thread of any kind is used, and calling [`ReadyToRun::run`] directly performs
//! the execution of the Wasm code. If you need parallelism, you are encouraged to spawn a
//! background thread yourself and call this function from there.
//!
//! If the runtime has finished, or has crashed, or wants to perform an operation with side
//! effects, then [`ReadyToRun::run`] will return and you must call [`WasmVm::state`] again to
//! determine what to do next. For example, if [`State::ExternalStorageGet`] is returned, then you
//! must load a value from the storage and pass it back by calling [`Resume::finish_call`].
//!
//! The Wasm execution is fully deterministic, and the outcome of the execution only depends on
//! the inputs. There is, for example, no implicit injection of randomness or of the current time.

mod allocator;
mod externals;
mod vm;

pub use externals::{
    CoreVersionSuccess, ExternalsVm as WasmVm, FunctionToCall, NonConformingErr, ReadyToRun,
    Resume, State, Success,
};
pub use vm::WasmBlob;
