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

//! Implements the API documented [in the parent module](..).

use super::{
    ExecOutcome, GlobalValueErr, HeapPages, ModuleError, NewErr, OutOfBoundsError, RunErr,
    Signature, StartErr, Trap, ValueType, WasmValue,
};

use alloc::{borrow::ToOwned as _, boxed::Box, format, string::ToString as _, sync::Arc, vec::Vec};
use core::{
    cell::RefCell,
    convert::{TryFrom, TryInto as _},
    fmt,
};
use wasmi::memory_units::ByteSize as _;

/// See [`super::Module`].
#[derive(Clone)]
pub struct Module {
    // Note: an `Arc` is used in order to expose the same API as wasmtime does. If in the future
    // wasmtime happened to no longer use internal reference counting, this `Arc` should be
    // removed.
    inner: Arc<wasmi::Module>,
}

impl Module {
    /// See [`super::Module::new`].
    pub fn new(module_bytes: impl AsRef<[u8]>) -> Result<Self, NewErr> {
        let module = wasmi::Module::from_buffer(module_bytes.as_ref())
            .map_err(|err| ModuleError(err.to_string()))
            .map_err(NewErr::ModuleError)?;

        Ok(Module {
            inner: Arc::new(module),
        })
    }
}

/// See [`super::VirtualMachinePrototype`].
pub struct InterpreterPrototype {
    /// Original module, with resolved imports.
    module: wasmi::ModuleRef,

    /// Memory of the module instantiation.
    ///
    /// Right now we only support one unique `Memory` object per process. This is it.
    /// Contains `None` if the process doesn't export any memory object, which means it doesn't
    /// use any memory.
    memory: Option<wasmi::MemoryRef>,

    /// Table of the indirect function calls.
    ///
    /// In Wasm, function pointers are in reality indices in a table called
    /// `__indirect_function_table`. This is this table, if it exists.
    indirect_table: Option<wasmi::TableRef>,
}

impl InterpreterPrototype {
    /// See [`super::VirtualMachinePrototype::new`].
    pub fn new(
        module: &Module,
        heap_pages: HeapPages,
        mut symbols: impl FnMut(&str, &str, &Signature) -> Result<usize, ()>,
    ) -> Result<Self, NewErr> {
        struct ImportResolve<'a> {
            functions: RefCell<&'a mut dyn FnMut(&str, &str, &Signature) -> Result<usize, ()>>,
            import_memory: RefCell<&'a mut Option<wasmi::MemoryRef>>,
            heap_pages: usize,
        }

        impl<'a> wasmi::ImportResolver for ImportResolve<'a> {
            fn resolve_func(
                &self,
                module_name: &str,
                field_name: &str,
                signature: &wasmi::Signature,
            ) -> Result<wasmi::FuncRef, wasmi::Error> {
                let closure = &mut **self.functions.borrow_mut();
                let conv_signature = match TryFrom::try_from(signature) {
                    Ok(i) => i,
                    Err(_) => {
                        return Err(wasmi::Error::Instantiation(format!(
                            "Function with unsupported signature `{}`:`{}`",
                            module_name, field_name
                        )))
                    }
                };
                let index = match closure(module_name, field_name, &conv_signature) {
                    Ok(i) => i,
                    Err(_) => {
                        return Err(wasmi::Error::Instantiation(format!(
                            "Couldn't resolve `{}`:`{}`",
                            module_name, field_name
                        )))
                    }
                };

                Ok(wasmi::FuncInstance::alloc_host(signature.clone(), index))
            }

            fn resolve_global(
                &self,
                _module_name: &str,
                _field_name: &str,
                _global_type: &wasmi::GlobalDescriptor,
            ) -> Result<wasmi::GlobalRef, wasmi::Error> {
                Err(wasmi::Error::Instantiation(
                    "Importing globals is not supported yet".to_owned(),
                ))
            }

            fn resolve_memory(
                &self,
                _module_name: &str,
                field_name: &str,
                memory_type: &wasmi::MemoryDescriptor,
            ) -> Result<wasmi::MemoryRef, wasmi::Error> {
                if field_name != "memory" {
                    return Err(wasmi::Error::Instantiation(format!(
                        "Unknown memory reference with name: {}",
                        field_name
                    )));
                }

                match &mut *self.import_memory.borrow_mut() {
                    Some(_) => Err(wasmi::Error::Instantiation(
                        "Memory can not be imported twice!".into(),
                    )),
                    memory_ref @ None => {
                        if memory_type
                            .maximum()
                            .map(|m| m.saturating_sub(memory_type.initial()))
                            .map(|m| self.heap_pages > m as usize)
                            .unwrap_or(false)
                        {
                            Err(wasmi::Error::Instantiation(format!(
                                "Heap pages ({}) is greater than imported memory maximum ({}).",
                                self.heap_pages,
                                memory_type
                                    .maximum()
                                    .map(|m| m.saturating_sub(memory_type.initial()))
                                    .unwrap(),
                            )))
                        } else {
                            let memory = wasmi::MemoryInstance::alloc(
                                wasmi::memory_units::Pages(
                                    memory_type.initial() as usize + self.heap_pages,
                                ),
                                Some(wasmi::memory_units::Pages(
                                    memory_type.initial() as usize + self.heap_pages,
                                )),
                            )?;
                            **memory_ref = Some(memory.clone());
                            Ok(memory)
                        }
                    }
                }
            }

            fn resolve_table(
                &self,
                _module_name: &str,
                _field_name: &str,
                _table_type: &wasmi::TableDescriptor,
            ) -> Result<wasmi::TableRef, wasmi::Error> {
                Err(wasmi::Error::Instantiation(
                    "Importing tables is not supported yet".to_owned(),
                ))
            }
        }

        let heap_pages = usize::try_from(u32::from(heap_pages)).unwrap_or(usize::max_value());

        let mut import_memory = None;
        let not_started = {
            let resolver = ImportResolve {
                functions: RefCell::new(&mut symbols),
                import_memory: RefCell::new(&mut import_memory),
                heap_pages,
            };
            wasmi::ModuleInstance::new(&module.inner, &resolver)
                .map_err(|err| ModuleError(err.to_string()))
                .map_err(NewErr::ModuleError)?
        };
        // TODO: explain `assert_no_start`
        let module = not_started.assert_no_start();

        let memory = if let Some(import_memory) = import_memory {
            Some(import_memory)
        } else if let Some(mem) = module.export_by_name("memory") {
            if let Some(mem) = mem.as_memory() {
                // TODO: use Result
                mem.grow(wasmi::memory_units::Pages(heap_pages));
                Some(mem.clone())
            } else {
                return Err(NewErr::MemoryIsntMemory);
            }
        } else {
            None
        };

        let indirect_table = if let Some(tbl) = module.export_by_name("__indirect_function_table") {
            if let Some(tbl) = tbl.as_table() {
                Some(tbl.clone())
            } else {
                return Err(NewErr::IndirectTableIsntTable);
            }
        } else {
            None
        };

        Ok(InterpreterPrototype {
            module,
            memory,
            indirect_table,
        })
    }

    /// See [`super::VirtualMachinePrototype::global_value`].
    pub fn global_value(&self, name: &str) -> Result<u32, GlobalValueErr> {
        let heap_base_val = self
            .module
            .export_by_name(name)
            .ok_or(GlobalValueErr::NotFound)?
            .as_global()
            .ok_or(GlobalValueErr::Invalid)?
            .get();

        match heap_base_val {
            wasmi::RuntimeValue::I32(v) => match u32::try_from(v) {
                Ok(v) => Ok(v),
                Err(_) => Err(GlobalValueErr::Invalid),
            },
            _ => Err(GlobalValueErr::Invalid),
        }
    }

    /// See [`super::VirtualMachinePrototype::start`].
    pub fn start(
        self,
        function_name: &str,
        params: &[WasmValue],
    ) -> Result<Interpreter, (StartErr, Self)> {
        let execution = match self.module.export_by_name(function_name) {
            Some(wasmi::ExternVal::Func(f)) => {
                // Try to convert the signature of the function to call, in order to make sure
                // that the type of parameters and return value are supported.
                if Signature::try_from(f.signature()).is_err() {
                    return Err((StartErr::SignatureNotSupported, self));
                }

                match wasmi::FuncInstance::invoke_resumable(
                    &f,
                    params
                        .iter()
                        .map(|v| wasmi::RuntimeValue::from(*v))
                        .collect::<Vec<_>>(),
                ) {
                    Ok(e) => e,
                    Err(err) => unreachable!("{:?}", err), // TODO:
                }
            }
            None => return Err((StartErr::FunctionNotFound, self)),
            _ => return Err((StartErr::NotAFunction, self)),
        };

        Ok(Interpreter {
            _module: self.module,
            memory: self.memory,
            execution: Some(execution),
            interrupted: false,
            indirect_table: self.indirect_table,
            is_poisoned: false,
        })
    }
}

// The fields related to `wasmi` do not implement `Send` because they use `std::rc::Rc`. `Rc`
// does not implement `Send` because incrementing/decrementing the reference counter from
// multiple threads simultaneously would be racy. It is however perfectly sound to move all the
// instances of `Rc`s at once between threads, which is what we're doing here.
//
// This importantly means that we should never return a `Rc` (even by reference) across the API
// boundary.
//
// For this reason, it would also be unsafe to implement `Clone` on `InterpreterPrototype`. A
// user could clone the `InterpreterPrototype` and send it to another thread, which would be
// undefined behaviour.
// TODO: really annoying to have to use unsafe code
unsafe impl Send for InterpreterPrototype {}

impl fmt::Debug for InterpreterPrototype {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("InterpreterPrototype").finish()
    }
}

/// See [`super::VirtualMachine`].
pub struct Interpreter {
    /// Original module, with resolved imports.
    _module: wasmi::ModuleRef,

    /// Memory of the module instantiation.
    ///
    /// Right now we only support one unique `Memory` object per process. This is it.
    /// Contains `None` if the process doesn't export any memory object, which means it doesn't
    /// use any memory.
    memory: Option<wasmi::MemoryRef>,

    /// Table of the indirect function calls.
    ///
    /// In Wasm, function pointers are in reality indices in a table called
    /// `__indirect_function_table`. This is this table, if it exists.
    indirect_table: Option<wasmi::TableRef>,

    /// Execution context of this virtual machine. This notably holds the program counter, state
    /// of the stack, and so on.
    ///
    /// This field is an `Option` because we need to be able to temporarily extract it. It must
    /// always be `Some`.
    execution: Option<wasmi::FuncInvocation<'static>>,

    /// If false, then one must call `execution.start_execution()` instead of `resume_execution()`.
    /// This is a particularity of the Wasm interpreter that we don't want to expose in our API.
    interrupted: bool,

    /// If true, the state machine is in a poisoned state and cannot run any code anymore.
    is_poisoned: bool,
}

impl Interpreter {
    /// See [`super::VirtualMachine::run`].
    pub fn run(&mut self, value: Option<WasmValue>) -> Result<ExecOutcome, RunErr> {
        let value = value.map(wasmi::RuntimeValue::from);

        struct DummyExternals;
        impl wasmi::Externals for DummyExternals {
            fn invoke_index(
                &mut self,
                index: usize,
                args: wasmi::RuntimeArgs,
            ) -> Result<Option<wasmi::RuntimeValue>, wasmi::Trap> {
                Err(wasmi::TrapKind::Host(Box::new(Interrupt {
                    index,
                    args: args.as_ref().to_vec(),
                }))
                .into())
            }
        }

        #[derive(Debug)]
        struct Interrupt {
            index: usize,
            args: Vec<wasmi::RuntimeValue>,
        }
        impl fmt::Display for Interrupt {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "Interrupt")
            }
        }
        impl wasmi::HostError for Interrupt {}

        if self.is_poisoned {
            return Err(RunErr::Poisoned);
        }

        let mut execution = match self.execution.take() {
            Some(e) => e,
            None => unreachable!(),
        };

        // Since the signature of the function is checked at initialization to be supported, it is
        // guaranteed that the conversions below won't panic.

        let result = if self.interrupted {
            let expected_ty = execution
                .resumable_value_type()
                .map(|v| ValueType::try_from(v).unwrap());
            let obtained_ty = value
                .as_ref()
                .map(|v| ValueType::try_from(v.value_type()).unwrap());
            if expected_ty != obtained_ty {
                return Err(RunErr::BadValueTy {
                    expected: expected_ty,
                    obtained: obtained_ty,
                });
            }
            execution.resume_execution(value, &mut DummyExternals)
        } else {
            if value.is_some() {
                return Err(RunErr::BadValueTy {
                    expected: None,
                    obtained: value
                        .as_ref()
                        .map(|v| ValueType::try_from(v.value_type()).unwrap()),
                });
            }
            self.interrupted = true;
            execution.start_execution(&mut DummyExternals)
        };

        match result {
            Ok(return_value) => {
                self.is_poisoned = true;
                Ok(ExecOutcome::Finished {
                    return_value: Ok(return_value.map(|r| WasmValue::try_from(r).unwrap())),
                })
            }
            Err(wasmi::ResumableError::AlreadyStarted) => unreachable!(),
            Err(wasmi::ResumableError::NotResumable) => unreachable!(),
            Err(wasmi::ResumableError::Trap(ref trap)) if trap.kind().is_host() => {
                let interrupt: &Interrupt = match trap.kind() {
                    wasmi::TrapKind::Host(err) => match err.downcast_ref() {
                        Some(e) => e,
                        None => unreachable!(),
                    },
                    _ => unreachable!(),
                };
                self.execution = Some(execution);
                Ok(ExecOutcome::Interrupted {
                    id: interrupt.index,
                    params: interrupt
                        .args
                        .iter()
                        .cloned()
                        .map(TryFrom::try_from)
                        .collect::<Result<_, _>>()
                        .unwrap(),
                })
            }
            Err(wasmi::ResumableError::Trap(err)) => {
                self.is_poisoned = true;
                Ok(ExecOutcome::Finished {
                    return_value: Err(Trap(err.to_string())),
                })
            }
        }
    }

    /// See [`super::VirtualMachine::memory_size`].
    pub fn memory_size(&self) -> u32 {
        let mem = match self.memory.as_ref() {
            Some(m) => m,
            None => return 0,
        };

        u32::try_from(mem.current_size().0 * wasmi::memory_units::Pages::byte_size().0).unwrap()
    }

    /// See [`super::VirtualMachine::read_memory`].
    pub fn read_memory(
        &'_ self,
        offset: u32,
        size: u32,
    ) -> Result<impl AsRef<[u8]> + '_, OutOfBoundsError> {
        let mem = match self.memory.as_ref() {
            Some(m) => m,
            None => {
                return if offset == 0 && size == 0 {
                    Ok(AccessOffset::Empty)
                } else {
                    Err(OutOfBoundsError)
                }
            }
        };

        let offset = usize::try_from(offset).map_err(|_| OutOfBoundsError)?;

        let max = offset
            .checked_add(size.try_into().map_err(|_| OutOfBoundsError)?)
            .ok_or(OutOfBoundsError)?;

        enum AccessOffset<T> {
            Enabled {
                access: T,
                offset: usize,
                max: usize,
            },
            Empty,
        }

        impl<T: AsRef<[u8]>> AsRef<[u8]> for AccessOffset<T> {
            fn as_ref(&self) -> &[u8] {
                if let AccessOffset::Enabled {
                    access,
                    offset,
                    max,
                } = self
                {
                    &access.as_ref()[*offset..*max]
                } else {
                    &[]
                }
            }
        }

        let access = mem.direct_access();
        if max > access.as_ref().len() {
            return Err(OutOfBoundsError);
        }

        Ok(AccessOffset::Enabled {
            access,
            offset,
            max,
        })
    }

    /// See [`super::VirtualMachine::write_memory`].
    pub fn write_memory(&mut self, offset: u32, value: &[u8]) -> Result<(), OutOfBoundsError> {
        let mem = match self.memory.as_ref() {
            Some(m) => m,
            None => {
                return if offset == 0 && value.is_empty() {
                    Ok(())
                } else {
                    Err(OutOfBoundsError)
                }
            }
        };

        mem.set(offset, value).map_err(|_| OutOfBoundsError)
    }

    /// See [`super::VirtualMachine::into_prototype`].
    pub fn into_prototype(self) -> InterpreterPrototype {
        // TODO: zero the memory

        InterpreterPrototype {
            module: self._module,
            memory: self.memory,
            indirect_table: self.indirect_table,
        }
    }
}

// The fields related to `wasmi` do not implement `Send` because they use `std::rc::Rc`. `Rc`
// does not implement `Send` because incrementing/decrementing the reference counter from
// multiple threads simultaneously would be racy. It is however perfectly sound to move all the
// instances of `Rc`s at once between threads, which is what we're doing here.
//
// This importantly means that we should never return a `Rc` (even by reference) across the API
// boundary.
// TODO: really annoying to have to use unsafe code
unsafe impl Send for Interpreter {}

impl fmt::Debug for Interpreter {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Interpreter").finish()
    }
}
