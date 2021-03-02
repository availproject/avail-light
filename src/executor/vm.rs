// Smoldot
// Copyright (C) 2019-2021  Parity Technologies (UK) Ltd.
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

//! General-purpose WebAssembly virtual machine.
//!
//! Contains code related to running a WebAssembly virtual machine. Contrary to
//! (`HostVm`)[super::host::HostVm], this module isn't aware of any of the host
//! functions available to Substrate runtimes. It only contains the code required to run a virtual
//! machine, with some adjustments explained below.
//!
//! # Usage
//!
//! Call [`VirtualMachinePrototype::new`] in order to parse and/or compile some WebAssembly code.
//! One of the parameters of this function is a function that is passed the name of functions
//! imported by the Wasm code, and must return an opaque `usize`. This `usize` doesn't have any
//! meaning, but will later be passed back to the user through [`ExecOutcome::Interrupted::id`]
//! when the corresponding function is called.
//!
//! The WebAssembly code can export functions in two different ways:
//!
//! - Some functions are exported through an `(export)` statement.
//!   See <https://webassembly.github.io/spec/core/bikeshed/#export-section%E2%91%A0>.
//! - Some functions are stored in a global table called `__indirect_function_table`, and are
//!   later referred to by their index in this table. This is how the concept of "function
//!   pointers" commonly found in low-level programming languages is translated in WebAssembly.
//!
//! > **Note**: At the time of writing, it isn't possible to call the second type of functions yet.
//!
//! Use [`VirtualMachinePrototype::start`] in order to start executing a function exported through
//! an `(export)` statement.
//!
//! Call [`VirtualMachine::run`] on the [`VirtualMachine`] returned by `start` in order to run the
//! WebAssembly code. The `run` method returns either if the function being called returns, or if
//! the WebAssembly code calls a host function. In the latter case, [`ExecOutcome::Interrupted`]
//! is returned and the virtual machine is now paused. Once the logic of the host function has
//! been executed, call `run` again, passing the return value of that host function.
//!
//! # About heap pages
//!
//! In the WebAssembly specifications, the memory available in the WebAssembly virtual machine has
//! an initial size and a maximum size. One of the instructions available in WebAssembly code is
//! [the `memory.grow` instruction](https://webassembly.github.io/spec/core/bikeshed/#-hrefsyntax-instr-memorymathsfmemorygrow),
//! which allows increasing the size of the memory.
//!
//! The Substrate/Polkadot runtime environment, however, differs. Rather than having a resizable
//! memory, memory has a fixed size that consists of its initial size plus a number of pages equal
//! to the value of `heap_pages` passed as parameter. It is forbidden for the WebAssembly code
//! to use `memory.grow`.
//!
//! See also the [`../externals`] module for more information about how memory works in the
//! context of the Substrate/Polkadot runtime.
//!
//! # About `__indirect_function_table`
//!
//! At initialization, the virtual machine will look for a table named `__indirect_function_table`.
//! If present, this table is expected to contain functions. These functions can then be referred
//! to by their index in this table. This is how the concept of "function pointers" commonly found
//! in programming languages is translated in WebAssembly.
//!
//! > **Note**: When compiling C, C++, Rust, or similar languages to WebAssembly, one must pass
//! >           the `--export-table` option to the LLVM linker in order for this symbol to be
//! >           exported.
//!
//! # About imported vs exported memory
//!
//! WebAssembly supports, in theory, addressing multiple different memory objects. The WebAssembly
//! module can declare memory in two ways:
//!
//! - Either by exporting a memory object in the `(export)` section under the name `memory`.
//! - Or by importing a memory object in its `(import)` section.
//!
//! The virtual machine in this module supports both variants. However, no more than one memory
//! object can be exported or imported.
//!
//! The first variant used to be the default model when compiling to WebAssembly, but the second
//! variant (importing memory objects) is preferred nowadays.

mod interpreter;
#[cfg(all(target_arch = "x86_64", feature = "std"))]
mod jit;

use alloc::{string::String, vec::Vec};
use core::{convert::TryFrom, fmt};
use smallvec::SmallVec;

/// Compiled Wasm code.
///
/// > **Note**: This struct implements `Clone`. The internals are reference-counted, meaning that
/// >           cloning is cheap.
#[derive(Clone)]
pub struct Module {
    inner: ModuleInner,
}

#[derive(Clone)]
enum ModuleInner {
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    Jit(jit::Module),
    Interpreter(interpreter::Module),
}

impl Module {
    /// Compiles the given Wasm code.
    pub fn new(module: impl AsRef<[u8]>, exec_hint: ExecHint) -> Result<Self, NewErr> {
        let use_wasmtime = match exec_hint {
            #[cfg(all(target_arch = "x86_64", feature = "std"))]
            ExecHint::CompileAheadOfTime => true,
            #[cfg(not(all(target_arch = "x86_64", feature = "std")))]
            ExecHint::CompileAheadOfTime => false,
            ExecHint::Oneshot | ExecHint::Untrusted => false,
        };

        Ok(Module {
            inner: if use_wasmtime {
                #[cfg(all(target_arch = "x86_64", feature = "std"))]
                let out = ModuleInner::Jit(jit::Module::new(module)?);
                #[cfg(not(all(target_arch = "x86_64", feature = "std")))]
                let out = unreachable!();
                out
            } else {
                ModuleInner::Interpreter(interpreter::Module::new(module)?)
            },
        })
    }
}

pub struct VirtualMachinePrototype {
    inner: VirtualMachinePrototypeInner,
}

enum VirtualMachinePrototypeInner {
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    Jit(jit::JitPrototype),
    Interpreter(interpreter::InterpreterPrototype),
}

impl VirtualMachinePrototype {
    /// Creates a new process state machine from the given module. This method notably allocates
    /// the memory necessary for the virtual machine to run.
    ///
    /// The closure is called for each import that the module has. It must assign a number to each
    /// import, or return an error if the import can't be resolved. When the VM calls one of these
    /// functions, this number will be returned back in order for the user to know how to handle
    /// the call.
    ///
    /// See [the module-level documentation](..) for an explanation of the parameters.
    pub fn new(
        module: &Module,
        heap_pages: HeapPages,
        symbols: impl FnMut(&str, &str, &Signature) -> Result<usize, ()>,
    ) -> Result<Self, NewErr> {
        Ok(VirtualMachinePrototype {
            inner: match &module.inner {
                ModuleInner::Interpreter(module) => VirtualMachinePrototypeInner::Interpreter(
                    interpreter::InterpreterPrototype::new(module, heap_pages, symbols)?,
                ),
                #[cfg(all(target_arch = "x86_64", feature = "std"))]
                ModuleInner::Jit(module) => VirtualMachinePrototypeInner::Jit(
                    jit::JitPrototype::new(module, heap_pages, symbols)?,
                ),
            },
        })
    }

    /// Returns the value of a global that the module exports.
    ///
    /// The global variable must be a `u32`, otherwise an error is returned.
    pub fn global_value(&mut self, name: &str) -> Result<u32, GlobalValueErr> {
        match &mut self.inner {
            #[cfg(all(target_arch = "x86_64", feature = "std"))]
            VirtualMachinePrototypeInner::Jit(inner) => inner.global_value(name),
            VirtualMachinePrototypeInner::Interpreter(inner) => inner.global_value(name),
        }
    }

    /// Turns this prototype into an actual virtual machine. This requires choosing which function
    /// to execute.
    pub fn start(
        mut self,
        function_name: &str,
        params: &[WasmValue],
    ) -> Result<VirtualMachine, (StartErr, Self)> {
        Ok(VirtualMachine {
            inner: match self.inner {
                #[cfg(all(target_arch = "x86_64", feature = "std"))]
                VirtualMachinePrototypeInner::Jit(inner) => {
                    match inner.start(function_name, params) {
                        Ok(vm) => VirtualMachineInner::Jit(vm),
                        Err((err, proto)) => {
                            self.inner = VirtualMachinePrototypeInner::Jit(proto);
                            return Err((err, self));
                        }
                    }
                }
                VirtualMachinePrototypeInner::Interpreter(inner) => {
                    match inner.start(function_name, params) {
                        Ok(vm) => VirtualMachineInner::Interpreter(vm),
                        Err((err, proto)) => {
                            self.inner = VirtualMachinePrototypeInner::Interpreter(proto);
                            return Err((err, self));
                        }
                    }
                }
            },
        })
    }
}

impl fmt::Debug for VirtualMachinePrototype {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.inner {
            #[cfg(all(target_arch = "x86_64", feature = "std"))]
            VirtualMachinePrototypeInner::Jit(inner) => fmt::Debug::fmt(inner, f),
            VirtualMachinePrototypeInner::Interpreter(inner) => fmt::Debug::fmt(inner, f),
        }
    }
}

pub struct VirtualMachine {
    inner: VirtualMachineInner,
}

enum VirtualMachineInner {
    #[cfg(all(target_arch = "x86_64", feature = "std"))]
    Jit(jit::Jit),
    Interpreter(interpreter::Interpreter),
}

impl VirtualMachine {
    /// Starts or continues execution of this thread.
    ///
    /// If this is the first call you call [`run`](VirtualMachine::run) for this thread, then you
    /// must pass a value of `None`.
    /// If, however, you call this function after a previous call to [`run`](VirtualMachine::run)
    /// that was interrupted by a host function call, then you must pass back the outcome of
    /// that call.
    pub fn run(&mut self, value: Option<WasmValue>) -> Result<ExecOutcome, RunErr> {
        match &mut self.inner {
            #[cfg(all(target_arch = "x86_64", feature = "std"))]
            VirtualMachineInner::Jit(inner) => inner.run(value),
            VirtualMachineInner::Interpreter(inner) => inner.run(value),
        }
    }

    /// Returns the size of the memory, in bytes.
    ///
    /// > **Note**: This can change over time if the Wasm code uses the `grow` opcode.
    pub fn memory_size(&self) -> u32 {
        match &self.inner {
            #[cfg(all(target_arch = "x86_64", feature = "std"))]
            VirtualMachineInner::Jit(inner) => inner.memory_size(),
            VirtualMachineInner::Interpreter(inner) => inner.memory_size(),
        }
    }

    /// Copies the given memory range into a `Vec<u8>`.
    ///
    /// Returns an error if the range is invalid or out of range.
    pub fn read_memory(
        &'_ self,
        offset: u32,
        size: u32,
    ) -> Result<impl AsRef<[u8]> + '_, OutOfBoundsError> {
        Ok(match &self.inner {
            #[cfg(all(target_arch = "x86_64", feature = "std"))]
            VirtualMachineInner::Jit(inner) => either::Left(inner.read_memory(offset, size)?),
            #[cfg(all(target_arch = "x86_64", feature = "std"))]
            VirtualMachineInner::Interpreter(inner) => {
                either::Right(inner.read_memory(offset, size)?)
            }
            #[cfg(not(all(target_arch = "x86_64", feature = "std")))]
            VirtualMachineInner::Interpreter(inner) => inner.read_memory(offset, size)?,
        })
    }

    /// Write the data at the given memory location.
    ///
    /// Returns an error if the range is invalid or out of range.
    pub fn write_memory(&mut self, offset: u32, value: &[u8]) -> Result<(), OutOfBoundsError> {
        match &mut self.inner {
            #[cfg(all(target_arch = "x86_64", feature = "std"))]
            VirtualMachineInner::Jit(inner) => inner.write_memory(offset, value),
            VirtualMachineInner::Interpreter(inner) => inner.write_memory(offset, value),
        }
    }

    /// Turns back this virtual machine into a prototype.
    pub fn into_prototype(self) -> VirtualMachinePrototype {
        VirtualMachinePrototype {
            inner: match self.inner {
                #[cfg(all(target_arch = "x86_64", feature = "std"))]
                VirtualMachineInner::Jit(inner) => {
                    VirtualMachinePrototypeInner::Jit(inner.into_prototype())
                }
                VirtualMachineInner::Interpreter(inner) => {
                    VirtualMachinePrototypeInner::Interpreter(inner.into_prototype())
                }
            },
        }
    }
}

impl fmt::Debug for VirtualMachine {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.inner {
            #[cfg(all(target_arch = "x86_64", feature = "std"))]
            VirtualMachineInner::Jit(inner) => fmt::Debug::fmt(inner, f),
            VirtualMachineInner::Interpreter(inner) => fmt::Debug::fmt(inner, f),
        }
    }
}

/// Hint used by the implementation to decide which kind of virtual machine to use.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ExecHint {
    /// The WebAssembly code will be instantiated once and run many times.
    /// If possible, compile this WebAssembly code ahead of time.
    CompileAheadOfTime,
    /// The WebAssembly code is expected to be only run once.
    ///
    /// > **Note**: This isn't a hard requirement but a hint.
    Oneshot,
    /// The WebAssembly code running through this VM is untrusted.
    Untrusted,
}

/// Number of heap pages available to the Wasm code.
// TODO: shouldn't that be in `host` module?
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HeapPages(u32);

impl HeapPages {
    pub const fn new(v: u32) -> Self {
        HeapPages(v)
    }
}

impl From<u32> for HeapPages {
    fn from(v: u32) -> Self {
        HeapPages(v)
    }
}

impl From<HeapPages> for u32 {
    fn from(v: HeapPages) -> Self {
        v.0
    }
}

/// Low-level Wasm function signature.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Signature {
    params: SmallVec<[ValueType; 8]>,
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
    fn from(sig: &'a Signature) -> Self {
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

impl<'a> TryFrom<&'a wasmi::Signature> for Signature {
    type Error = UnsupportedTypeError;

    fn try_from(sig: &'a wasmi::Signature) -> Result<Self, Self::Error> {
        Ok(Signature {
            params: sig
                .params()
                .iter()
                .cloned()
                .map(ValueType::try_from)
                .collect::<Result<_, _>>()?,
            ret_ty: sig.return_type().map(ValueType::try_from).transpose()?,
        })
    }
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
impl<'a> TryFrom<&'a wasmtime::FuncType> for Signature {
    type Error = UnsupportedTypeError;

    fn try_from(sig: &'a wasmtime::FuncType) -> Result<Self, Self::Error> {
        if sig.results().len() > 1 {
            return Err(UnsupportedTypeError);
        }

        Ok(Signature {
            params: sig
                .params()
                .map(ValueType::try_from)
                .collect::<Result<_, _>>()?,
            ret_ty: sig.results().next().map(ValueType::try_from).transpose()?,
        })
    }
}

impl TryFrom<wasmi::Signature> for Signature {
    type Error = UnsupportedTypeError;

    fn try_from(sig: wasmi::Signature) -> Result<Self, Self::Error> {
        Signature::try_from(&sig)
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

impl TryFrom<wasmi::RuntimeValue> for WasmValue {
    type Error = UnsupportedTypeError;

    fn try_from(val: wasmi::RuntimeValue) -> Result<Self, Self::Error> {
        match val {
            wasmi::RuntimeValue::I32(v) => Ok(WasmValue::I32(v)),
            wasmi::RuntimeValue::I64(v) => Ok(WasmValue::I64(v)),
            _ => Err(UnsupportedTypeError),
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

#[cfg(all(target_arch = "x86_64", feature = "std"))]
impl From<WasmValue> for wasmtime::Val {
    fn from(val: WasmValue) -> Self {
        match val {
            WasmValue::I32(v) => wasmtime::Val::I32(v),
            WasmValue::I64(v) => wasmtime::Val::I64(v),
        }
    }
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
impl<'a> TryFrom<&'a wasmtime::Val> for WasmValue {
    type Error = UnsupportedTypeError;

    fn try_from(val: &'a wasmtime::Val) -> Result<Self, Self::Error> {
        match val {
            wasmtime::Val::I32(v) => Ok(WasmValue::I32(*v)),
            wasmtime::Val::I64(v) => Ok(WasmValue::I64(*v)),
            _ => Err(UnsupportedTypeError),
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

impl TryFrom<wasmi::ValueType> for ValueType {
    type Error = UnsupportedTypeError;

    fn try_from(val: wasmi::ValueType) -> Result<Self, Self::Error> {
        match val {
            wasmi::ValueType::I32 => Ok(ValueType::I32),
            wasmi::ValueType::I64 => Ok(ValueType::I64),
            _ => Err(UnsupportedTypeError),
        }
    }
}

#[cfg(all(target_arch = "x86_64", feature = "std"))]
impl TryFrom<wasmtime::ValType> for ValueType {
    type Error = UnsupportedTypeError;

    fn try_from(val: wasmtime::ValType) -> Result<Self, Self::Error> {
        match val {
            wasmtime::ValType::I32 => Ok(ValueType::I32),
            wasmtime::ValType::I64 => Ok(ValueType::I64),
            _ => Err(UnsupportedTypeError),
        }
    }
}

/// Error used in the conversions between VM implementation and the public API.
#[derive(Debug, derive_more::Display)]
pub struct UnsupportedTypeError;

/// Outcome of the [`run`](VirtualMachine::run) function.
#[derive(Debug)]
pub enum ExecOutcome {
    /// The execution has finished.
    ///
    /// The state machine is now in a poisoned state, and calling [`run`](VirtualMachine::run)
    /// will return [`RunErr::Poisoned`].
    Finished {
        /// Return value of the function.
        return_value: Result<Option<WasmValue>, Trap>,
    },

    /// The virtual machine has been paused due to a call to a host function.
    ///
    /// This variant contains the identifier of the host function that is expected to be
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

/// Opaque error that happened during execution, such as an `unreachable` instruction.
#[derive(Debug, derive_more::Display, Clone)]
#[display(fmt = "{}", _0)]
pub struct Trap(String);

/// Error that can happen when initializing a [`VirtualMachinePrototype`].
#[derive(Debug, derive_more::Display)]
pub enum NewErr {
    /// Error while parsing or compiling the WebAssembly code.
    #[display(fmt = "{}", _0)]
    ModuleError(ModuleError),
    /// If a "memory" symbol is provided, it must be a memory.
    #[display(fmt = "If a \"memory\" symbol is provided, it must be a memory.")]
    MemoryIsntMemory,
    /// If a "__indirect_function_table" symbol is provided, it must be a table.
    #[display(fmt = "If a \"__indirect_function_table\" symbol is provided, it must be a table.")]
    IndirectTableIsntTable,
}

/// Error that can happen when calling [`VirtualMachinePrototype::start`].
#[derive(Debug, Clone, derive_more::Display)]
pub enum StartErr {
    /// Couldn't find the requested function.
    #[display(fmt = "Function to start was not found.")]
    FunctionNotFound,
    /// The requested function has been found in the list of exports, but it is not a function.
    #[display(fmt = "Symbol to start is not a function.")]
    NotAFunction,
    /// The requested function has a signature that isn't supported.
    #[display(fmt = "Function to start uses unsupported signature.")]
    SignatureNotSupported,
}

/// Opaque error indicating an error while parsing or compiling the WebAssembly code.
#[derive(Debug, derive_more::Display)]
#[display(fmt = "{}", _0)]
pub struct ModuleError(String);

/// Error while reading memory.
#[derive(Debug, derive_more::Display)]
#[display(fmt = "Out of bounds when accessing virtual machine memory")]
pub struct OutOfBoundsError;

/// Error that can happen when resuming the execution of a function.
#[derive(Debug, derive_more::Display)]
pub enum RunErr {
    /// The state machine is poisoned.
    #[display(fmt = "State machine is poisoned")]
    Poisoned,
    /// Passed a wrong value back.
    #[display(
        fmt = "Expected value of type {:?} but got {:?} instead",
        expected,
        obtained
    )]
    BadValueTy {
        /// Type of the value that was expected.
        expected: Option<ValueType>,
        /// Type of the value that was actually passed.
        obtained: Option<ValueType>,
    },
}

/// Error that can happen when calling [`VirtualMachinePrototype::global_value`].
#[derive(Debug, derive_more::Display)]
pub enum GlobalValueErr {
    /// Couldn't find requested symbol.
    NotFound,
    /// Requested symbol isn't a `u32`.
    Invalid,
}

#[cfg(test)]
mod tests {
    // TODO:

    #[test]
    fn is_send() {
        // Makes sure that the virtual machine types implement `Send`.
        fn test<T: Send>() {}
        test::<super::VirtualMachine>();
        test::<super::VirtualMachinePrototype>();
    }
}
