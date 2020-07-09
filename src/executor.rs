//! Wasm runtime code execution.
//!
//! WebAssembly (often abbreviated *Wasm*) plays a big role in Substrate/Polkadot. The storage of
//! each block in the chain has a special key named `:code` which contains the WebAssembly code
//! of what we call *the runtime*.
//!
//! The runtime is a program (in WebAssembly) that decides, amongst other things, whether
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
//! While the Wasm runtime code has side-effects (such as storing values in the storage), the
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
//!
//! # Example
//!
//! ```no_run
//! // Create the `WasmBlob`.
//! let wasm_binary: &[u8] = unimplemented!();
//! let wasm_blob = substrate_lite::executor::WasmBlob::from_bytes(wasm_binary).unwrap();
//!
//! // Start executing a function on the runtime.
//! let vm = {
//!     let to_call = substrate_lite::executor::FunctionToCall::CoreVersion;
//!     // Errors can happen only if the Wasm code is wrong/non-conforming, or if the input data
//!     // can't fit in the virtual machine (which usually means that it is blatently wrong).
//!     substrate_lite::executor::WasmVm::new(&wasm_blob, to_call).unwrap()
//! };
//!
//! // We need to answer the calls that the runtime might perform.
//! loop {
//!     match vm.state() {
//!         // Calling `runner.run()` is what actually executes WebAssembly code and updates
//!         // the state.
//!         substrate_lite::executor::State::ReadyToRun(runner) => runner.run(),
//!
//!         substrate_lite::executor::State::Finished(value) => {
//!             // `value` here is an enum of type `Success`, whose variant depends on which
//!             // function has been called. Since we call `CoreVersion`, we know that this is
//!             // a `CoreVersion` return value.
//!             let info = match value {
//!                 substrate_lite::executor::Success::CoreVersion(info) => info,
//!                 _ => unreachable!()
//!             };
//!
//!             println!("Success! Returned value: {:?}", info);
//!             break;
//!         },
//!
//!         // Errors can happen if the WebAssembly code panics or does something wrong.
//!         // In a real-life situation, the host should obviously not panic in these situations.
//!         substrate_lite::executor::State::NonConforming(_) |
//!         substrate_lite::executor::State::Trapped => panic!("Error while executing code"),
//!
//!         // All the other variants correspond to function calls that the runtime might perform.
//!         // `ExternalStorageGet` is shown here as an example.
//!         substrate_lite::executor::State::ExternalStorageGet { storage_key, resolve, .. } => {
//!             println!("Runtime wants to read the storage at {:?}", storage_key);
//!             // Injects the value into the virtual machine and updates the state.
//!             resolve.finish_call(None); // Just a stub
//!         }
//!         _ => unimplemented!()
//!     }
//! }
//! ```
// TODO: use an actual Wasm blob extracted from somewhere as an example ^

mod allocator;
mod externals;
mod vm;

pub use externals::{
    CoreVersionSuccess, ExternalsVm as WasmVm, ExternalsVmPrototype as WasmVmPrototype,
    FunctionToCall, NonConformingErr, ReadyToRun, Resume, State, Success,
};
