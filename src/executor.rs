// TODO: docs

mod allocator;
mod externals;
mod vm;

pub use externals::{
    CoreVersionSuccess, ExternalsVm as WasmVm, FunctionToCall, NonConformingErr, State,
    StateReadyToRun, StateWaitExternalResolve, Success,
};
pub use vm::WasmBlob;
