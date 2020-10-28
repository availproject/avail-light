// Substrate-lite
// Copyright (C) 2019-2020  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

mod interpreter;
#[cfg(target_arch = "x86_64")]
mod jit;

use alloc::vec::Vec;
use core::fmt;
use smallvec::SmallVec;

#[cfg(target_arch = "x86_64")]
pub use jit::{Jit as VirtualMachine, JitPrototype as VirtualMachinePrototype};

#[cfg(not(target_arch = "x86_64"))]
pub use interpreter::*;

// TODO: wrap around the content of the submodules here, and make the submodules private

/// Low-level Wasm function signature.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Signature {
    params: SmallVec<[ValueType; 2]>,
    ret_ty: Option<ValueType>,
}

impl Signature {
    /// Creates a [`Signature`] from the given parameter types and return type.
    pub fn new(
        params: impl Iterator<Item = ValueType>,
        ret_ty: impl Into<Option<ValueType>>,
    ) -> Signature {
        Signature {
            params: params.collect(),
            ret_ty: ret_ty.into(),
        }
    }

    /// Returns a list of all the types of the parameters.
    pub fn parameters(&self) -> impl ExactSizeIterator<Item = &ValueType> {
        self.params.iter()
    }

    /// Returns the type of the return type of the function. `None` means "void".
    pub fn return_type(&self) -> Option<&ValueType> {
        self.ret_ty.as_ref()
    }
}

impl<'a> From<&'a Signature> for wasmi::Signature {
    fn from(sig: &'a Signature) -> wasmi::Signature {
        wasmi::Signature::new(
            sig.params
                .iter()
                .cloned()
                .map(wasmi::ValueType::from)
                .collect::<Vec<_>>(),
            sig.ret_ty.map(wasmi::ValueType::from),
        )
    }
}

impl From<Signature> for wasmi::Signature {
    fn from(sig: Signature) -> wasmi::Signature {
        wasmi::Signature::from(&sig)
    }
}

impl<'a> From<&'a wasmi::Signature> for Signature {
    fn from(sig: &'a wasmi::Signature) -> Signature {
        Signature::new(
            sig.params().iter().cloned().map(ValueType::from),
            sig.return_type().map(ValueType::from),
        )
    }
}

#[cfg(target_arch = "x86_64")]
impl<'a> From<&'a wasmtime::FuncType> for Signature {
    fn from(sig: &'a wasmtime::FuncType) -> Signature {
        // TODO: we only support one return type at the moment; what even is multiple
        // return types?
        assert!(sig.results().len() <= 1);

        Signature::new(
            sig.params().iter().cloned().map(ValueType::from),
            sig.results().get(0).cloned().map(ValueType::from),
        )
    }
}

impl From<wasmi::Signature> for Signature {
    fn from(sig: wasmi::Signature) -> Signature {
        Signature::from(&sig)
    }
}

/// Value that a Wasm function can accept or produce.
#[derive(Debug, Copy, Clone)]
pub enum WasmValue {
    /// A 32-bits integer. There is no fundamental difference between signed and unsigned
    /// integer, and the signed-ness should be determined depending on the context.
    I32(i32),
    /// A 32-bits integer. There is no fundamental difference between signed and unsigned
    /// integer, and the signed-ness should be determined depending on the context.
    I64(i64),
}

/// Type of a value passed as parameter or returned by a function.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum ValueType {
    /// A 32-bits integer. Used for both signed and unsigned integers.
    I32,
    /// A 64-bits integer. Used for both signed and unsigned integers.
    I64,
}

impl WasmValue {
    /// Returns the type corresponding to this value.
    pub fn ty(&self) -> ValueType {
        match self {
            WasmValue::I32(_) => ValueType::I32,
            WasmValue::I64(_) => ValueType::I64,
        }
    }

    /// Unwraps [`WasmValue::I32`] into its value.
    pub fn into_i32(self) -> Option<i32> {
        if let WasmValue::I32(v) = self {
            Some(v)
        } else {
            None
        }
    }

    /// Unwraps [`WasmValue::I64`] into its value.
    pub fn into_i64(self) -> Option<i64> {
        if let WasmValue::I64(v) = self {
            Some(v)
        } else {
            None
        }
    }
}

impl From<wasmi::RuntimeValue> for WasmValue {
    fn from(val: wasmi::RuntimeValue) -> Self {
        match val {
            wasmi::RuntimeValue::I32(v) => WasmValue::I32(v),
            wasmi::RuntimeValue::I64(v) => WasmValue::I64(v),
            _ => panic!(), // TODO: do something other than panicking here
        }
    }
}

impl From<WasmValue> for wasmi::RuntimeValue {
    fn from(val: WasmValue) -> Self {
        match val {
            WasmValue::I32(v) => wasmi::RuntimeValue::I32(v),
            WasmValue::I64(v) => wasmi::RuntimeValue::I64(v),
        }
    }
}

#[cfg(target_arch = "x86_64")]
impl From<WasmValue> for wasmtime::Val {
    fn from(val: WasmValue) -> Self {
        match val {
            WasmValue::I32(v) => wasmtime::Val::I32(v),
            WasmValue::I64(v) => wasmtime::Val::I64(v),
        }
    }
}

#[cfg(target_arch = "x86_64")]
impl From<wasmtime::Val> for WasmValue {
    fn from(val: wasmtime::Val) -> Self {
        match val {
            wasmtime::Val::I32(v) => WasmValue::I32(v),
            wasmtime::Val::I64(v) => WasmValue::I64(v),
            _ => unimplemented!(),
        }
    }
}

impl From<ValueType> for wasmi::ValueType {
    fn from(ty: ValueType) -> wasmi::ValueType {
        match ty {
            ValueType::I32 => wasmi::ValueType::I32,
            ValueType::I64 => wasmi::ValueType::I64,
        }
    }
}

impl From<wasmi::ValueType> for ValueType {
    fn from(val: wasmi::ValueType) -> Self {
        match val {
            wasmi::ValueType::I32 => ValueType::I32,
            wasmi::ValueType::I64 => ValueType::I64,
            _ => panic!(), // TODO: do something other than panicking here
        }
    }
}

#[cfg(target_arch = "x86_64")]
impl From<wasmtime::ValType> for ValueType {
    fn from(val: wasmtime::ValType) -> Self {
        match val {
            wasmtime::ValType::I32 => ValueType::I32,
            wasmtime::ValType::I64 => ValueType::I64,
            _ => unimplemented!(), // TODO:
        }
    }
}

/// Outcome of the [`run`](VirtualMachine::run) function.
#[derive(Debug)]
pub enum ExecOutcome {
    /// The execution has finished.
    ///
    /// The state machine is now in a poisoned state, and calling
    /// [`is_poisoned`](VirtualMachine::is_poisoned) will return true.
    Finished {
        /// Return value of the function.
        // TODO: error type should change here
        return_value: Result<Option<WasmValue>, ()>,
    },

    /// The virtual machine has been paused due to a call to an external function.
    ///
    /// This variant contains the identifier of the external function that is expected to be
    /// called, and its parameters. When you call [`run`](VirtualMachine::run) again, you must
    /// pass back the outcome of calling that function.
    ///
    /// > **Note**: The type of the return value of the function is called is not specified, as the
    /// >           user is supposed to know it based on the identifier. It is an error to call
    /// >           [`run`](VirtualMachine::run) with a value of the wrong type.
    Interrupted {
        /// Identifier of the function to call. Corresponds to the value provided at
        /// initialization when resolving imports.
        id: usize,

        /// Parameters of the function call.
        params: Vec<WasmValue>,
    },
}
/// Error that can happen when initializing a VM.
#[derive(Debug)]
pub enum NewErr {
    /// Error in the interpreter.
    // TODO: don't expose wasmi in API
    Interpreter(wasmi::Error),
    /// If a "memory" symbol is provided, it must be a memory.
    MemoryIsntMemory,
    /// If a "__indirect_function_table" symbol is provided, it must be a table.
    IndirectTableIsntTable,
    /// Couldn't find the requested function.
    FunctionNotFound,
    /// The requested function has been found in the list of exports, but it is not a function.
    NotAFunction,
}

impl fmt::Display for NewErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            NewErr::Interpreter(_) => write!(f, "Error in the interpreter"),
            NewErr::MemoryIsntMemory => {
                write!(f, "If a \"memory\" symbol is provided, it must be a memory")
            }
            NewErr::IndirectTableIsntTable => write!(
                f,
                "If a \"__indirect_function_table\" symbol is provided, it must be a table"
            ),
            NewErr::FunctionNotFound => write!(f, "Function to start was not found"),
            NewErr::NotAFunction => write!(f, "Symbol to start is not a function"),
        }
    }
}

/// Error that can happen when starting a new thread.
#[derive(Debug)]
pub enum StartErr {
    /// The state machine is poisoned and cannot run anymore.
    Poisoned,
    /// Couldn't find the requested function.
    FunctionNotFound,
    /// The requested function has been found in the list of exports, but it is not a function.
    NotAFunction,
}

impl fmt::Display for StartErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            StartErr::Poisoned => write!(f, "State machine is in a poisoned state"),
            StartErr::FunctionNotFound => write!(f, "Function to start was not found"),
            StartErr::NotAFunction => write!(f, "Symbol to start is not a function"),
        }
    }
}

/// Error that can happen when resuming the execution of a function.
#[derive(Debug)]
pub enum RunErr {
    /// The state machine is poisoned.
    Poisoned,
    /// Passed a wrong value back.
    BadValueTy {
        /// Type of the value that was expected.
        expected: Option<ValueType>,
        /// Type of the value that was actually passed.
        obtained: Option<ValueType>,
    },
}

impl fmt::Display for RunErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RunErr::Poisoned => write!(f, "State machine is poisoned"),
            RunErr::BadValueTy { expected, obtained } => write!(
                f,
                "Expected value of type {:?} but got {:?} instead",
                expected, obtained
            ),
        }
    }
}

/// Error that can happen when calling [`VirtualMachinePrototype::global_value`].
#[derive(Debug, derive_more::Display)]
pub enum GlobalValueErr {
    NotFound,
    Invalid,
}

#[cfg(test)]
mod tests {
    // TODO:
}
