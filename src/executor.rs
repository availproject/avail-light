// TODO: docs

mod allocator;
mod externals;
mod vm;

pub use externals::{
    CoreVersionSuccess, ExternalsVm as WasmVm, FunctionToCall, NonConformingErr, ReadyToRun,
    Resume, State, Success,
};
pub use vm::WasmBlob;
