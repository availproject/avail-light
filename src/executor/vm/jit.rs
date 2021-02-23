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
    Signature, StartErr, Trap, WasmValue,
};

use alloc::{boxed::Box, string::String, vec::Vec};
use core::{
    cmp,
    convert::{Infallible, TryFrom},
    fmt,
};

/// See [`super::VirtualMachinePrototype`].
pub struct JitPrototype {
    /// Coroutine that contains the Wasm execution stack.
    coroutine: corooteen::Coroutine<Box<dyn FnOnce() -> Infallible>, FromCoroutine, ToCoroutine>,

    /// Reference to the memory used by the module, if any.
    memory: Option<wasmtime::Memory>,

    /// Reference to the table of indirect functions, in case we need to access it.
    /// `None` if the module doesn't export such table.
    indirect_table: Option<wasmtime::Table>,
}

impl JitPrototype {
    /// See [`super::VirtualMachinePrototype::new`].
    pub fn new(
        module: impl AsRef<[u8]>,
        heap_pages: HeapPages,
        mut symbols: impl FnMut(&str, &str, &Signature) -> Result<usize, ()>,
    ) -> Result<Self, NewErr> {
        let mut config = wasmtime::Config::new();
        config.cranelift_nan_canonicalization(true);
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);
        let engine = wasmtime::Engine::new(&config);

        let store = wasmtime::Store::new(&engine);
        let module = wasmtime::Module::from_binary(&engine, module.as_ref())
            .map_err(|err| NewErr::ModuleError(ModuleError(err.to_string())))?;

        let builder = corooteen::CoroutineBuilder::new();

        let mut imported_memory = None;

        // Building the list of symbols that the Wasm VM is able to use.
        let imports = {
            let mut imports = Vec::with_capacity(module.imports().len());
            for import in module.imports() {
                match import.ty() {
                    wasmtime::ExternType::Func(f) => {
                        // TODO: don't panic below
                        let function_index = match import.name().and_then(|name| {
                            symbols(import.module(), name, &TryFrom::try_from(&f).unwrap()).ok()
                        }) {
                            Some(idx) => idx,
                            None => {
                                return Err(NewErr::ModuleError(ModuleError(format!(
                                    "unresolved import: `{}`:`{}`",
                                    import.module(),
                                    import.name().unwrap_or("<unnamed>")
                                ))));
                            }
                        };

                        let interrupter = builder.interrupter();
                        imports.push(wasmtime::Extern::Func(wasmtime::Func::new(
                            &store,
                            f.clone(),
                            move |_, params, ret_val| {
                                // This closure is executed whenever the Wasm VM calls a host function.
                                let returned = interrupter.interrupt(FromCoroutine::Interrupt {
                                    function_index,
                                    // Since function signatures are checked at initialization,
                                    // it is guaranteed that the parameter types are also
                                    // supported.
                                    parameters: params
                                        .iter()
                                        .map(TryFrom::try_from)
                                        .collect::<Result<_, _>>()
                                        .unwrap(),
                                });
                                let returned = match returned {
                                    ToCoroutine::Resume(returned) => returned,
                                    _ => unreachable!(),
                                };
                                if let Some(returned) = returned {
                                    assert_eq!(ret_val.len(), 1);
                                    ret_val[0] = From::from(returned);
                                } else {
                                    assert!(ret_val.is_empty());
                                }
                                Ok(())
                            },
                        )));
                    }
                    wasmtime::ExternType::Global(_)
                    | wasmtime::ExternType::Table(_)
                    | wasmtime::ExternType::Instance(_)
                    | wasmtime::ExternType::Module(_) => {
                        return Err(NewErr::ModuleError(ModuleError(
                            "global/table/instance/module imports not supported".to_string(),
                        )));
                    }
                    wasmtime::ExternType::Memory(m) => {
                        let limits = {
                            let heap_pages = u32::from(heap_pages);
                            let min = cmp::max(m.limits().min(), heap_pages);
                            let max = m.limits().max(); // TODO: make sure it's > to min, otherwise error
                            let num = min + heap_pages;
                            wasmtime::Limits::new(num, Some(num))
                        };

                        // TODO: check name and all?
                        // TODO: proper error instead of asserting?
                        assert!(imported_memory.is_none());
                        imported_memory = Some(wasmtime::Memory::new(
                            &store,
                            wasmtime::MemoryType::new(limits),
                        ));
                        imports.push(wasmtime::Extern::Memory(
                            imported_memory.as_ref().unwrap().clone(),
                        ));
                    }
                };
            }
            imports
        };

        // TODO: don't unwrap
        let instance = wasmtime::Instance::new(&store, &module, &imports).unwrap();

        let exported_memory = if let Some(mem) = instance.get_export("memory") {
            if let Some(mem) = mem.into_memory() {
                // TODO: do this properly
                mem.grow(u32::try_from(heap_pages).unwrap()).unwrap();
                Some(mem)
            } else {
                return Err(NewErr::MemoryIsntMemory);
            }
        } else {
            None
        };

        let memory = match (exported_memory, imported_memory) {
            (Some(_), Some(_)) => todo!(),
            (Some(m), None) => Some(m),
            (None, Some(m)) => Some(m),
            (None, None) => None,
        };

        let indirect_table = if let Some(tbl) = instance.get_export("__indirect_function_table") {
            if let Some(tbl) = tbl.into_table() {
                Some(tbl)
            } else {
                return Err(NewErr::IndirectTableIsntTable);
            }
        } else {
            None
        };

        // We now build the coroutine of the main thread.
        let mut coroutine = {
            let interrupter = builder.interrupter();
            builder.build(Box::new(move || -> Infallible {
                let mut request = interrupter.interrupt(FromCoroutine::Init);

                loop {
                    let (start_function_name, start_parameters) = loop {
                        match request {
                            ToCoroutine::Start(n, p) => break (n, p),
                            ToCoroutine::GetGlobal(global) => {
                                let global_val = match instance.get_export(&global) {
                                    Some(wasmtime::Extern::Global(g)) => match g.get() {
                                        wasmtime::Val::I32(v) => {
                                            Ok(u32::from_ne_bytes(v.to_ne_bytes()))
                                        }
                                        _ => Err(GlobalValueErr::Invalid),
                                    },
                                    _ => Err(GlobalValueErr::NotFound),
                                };

                                request = interrupter
                                    .interrupt(FromCoroutine::GetGlobalResponse(global_val));
                            }
                            ToCoroutine::Resume(_) => unreachable!(),
                        }
                    };

                    // Try to start executing `_start`.
                    let start_function = if let Some(f) = instance.get_export(&start_function_name)
                    {
                        if let Some(f) = f.into_func() {
                            f.clone()
                        } else {
                            let err = StartErr::NotAFunction;
                            request = interrupter.interrupt(FromCoroutine::StartResult(Err(err)));
                            continue;
                        }
                    } else {
                        let err = StartErr::FunctionNotFound;
                        request = interrupter.interrupt(FromCoroutine::StartResult(Err(err)));
                        continue;
                    };

                    // Try to convert the signature of the function to call, in order to make sure
                    // that the type of parameters and return value are supported.
                    if Signature::try_from(&start_function.ty()).is_err() {
                        request = interrupter.interrupt(FromCoroutine::StartResult(Err(
                            StartErr::SignatureNotSupported,
                        )));
                        continue;
                    }

                    // Report back that everything went ok.
                    let reinjected: ToCoroutine =
                        interrupter.interrupt(FromCoroutine::StartResult(Ok(())));
                    assert!(matches!(reinjected, ToCoroutine::Resume(None)));

                    // Now running the `start` function of the Wasm code.
                    // This will interrupt the coroutine every time a host function is reached.
                    let result = start_function.call(
                        &start_parameters
                            .into_iter()
                            .map(From::from)
                            .collect::<Vec<_>>(),
                    );

                    let result = match result {
                        Ok(r) => r,
                        Err(err) => {
                            request =
                                interrupter.interrupt(FromCoroutine::Done(Err(err.to_string())));
                            continue;
                        }
                    };

                    // Execution resumes here when the Wasm code has gracefully finished.
                    // The signature of the function has been chedk earlier, and as such it is
                    // guaranteed that it has only one return value.
                    assert!(result.len() == 0 || result.len() == 1);
                    let result = Ok(result.get(0).map(|v| TryFrom::try_from(v).unwrap()));
                    request = interrupter.interrupt(FromCoroutine::Done(result));
                }
            }) as Box<_>)
        };

        // Execute the coroutine once, as described above.
        // The first yield must always be an `FromCoroutine::Init`.
        match coroutine.run(None) {
            corooteen::RunOut::Interrupted(FromCoroutine::Init) => {}
            _ => unreachable!(),
        }

        Ok(JitPrototype {
            coroutine,
            memory,
            indirect_table,
        })
    }

    /// See [`super::VirtualMachinePrototype::global_value`].
    pub fn global_value(&mut self, name: &str) -> Result<u32, GlobalValueErr> {
        match self
            .coroutine
            .run(Some(ToCoroutine::GetGlobal(name.to_owned())))
        {
            corooteen::RunOut::Interrupted(FromCoroutine::GetGlobalResponse(outcome)) => outcome,
            _ => unreachable!(),
        }
    }

    /// See [`super::VirtualMachinePrototype::start`].
    pub fn start(mut self, function_name: &str, params: &[WasmValue]) -> Result<Jit, StartErr> {
        match self.coroutine.run(Some(ToCoroutine::Start(
            function_name.to_owned(),
            params.to_owned(),
        ))) {
            corooteen::RunOut::Interrupted(FromCoroutine::StartResult(Err(err))) => {
                return Err(err)
            }
            corooteen::RunOut::Interrupted(FromCoroutine::StartResult(Ok(()))) => {}
            _ => unreachable!(),
        }

        Ok(Jit {
            coroutine: self.coroutine,
            memory: self.memory,
            indirect_table: self.indirect_table,
        })
    }
}

// The fields related to `wasmtime` do not implement `Send` because they use `std::rc::Rc`. `Rc`
// does not implement `Send` because incrementing/decrementing the reference counter from
// multiple threads simultaneously would be racy. It is however perfectly sound to move all the
// instances of `Rc`s at once between threads, which is what we're doing here.
//
// This importantly means that we should never return a `Rc` (even by reference) across the API
// boundary.
// TODO: really annoying to have to use unsafe code
unsafe impl Send for JitPrototype {}

impl fmt::Debug for JitPrototype {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("JitPrototype").finish()
    }
}

/// See [`super::VirtualMachine`].
pub struct Jit {
    /// Coroutine that contains the Wasm execution stack.
    coroutine: corooteen::Coroutine<Box<dyn FnOnce() -> Infallible>, FromCoroutine, ToCoroutine>,

    /// See [`JitPrototype::memory`].
    memory: Option<wasmtime::Memory>,

    /// See [`JitPrototype::indirect_table`].
    indirect_table: Option<wasmtime::Table>,
}

impl Jit {
    /// See [`super::VirtualMachine::run`].
    pub fn run(&mut self, value: Option<WasmValue>) -> Result<ExecOutcome, RunErr> {
        if self.coroutine.is_finished() {
            return Err(RunErr::Poisoned);
        }

        // TODO: check value type

        // Resume the coroutine execution.
        match self
            .coroutine
            .run(Some(ToCoroutine::Resume(value.map(From::from))))
        {
            corooteen::RunOut::Finished(_void) => match _void {},

            corooteen::RunOut::Interrupted(FromCoroutine::Done(Err(err))) => {
                Ok(ExecOutcome::Finished {
                    return_value: Err(Trap(err)),
                })
            }
            corooteen::RunOut::Interrupted(FromCoroutine::Done(Ok(val))) => {
                Ok(ExecOutcome::Finished {
                    // Since we verify at initialization that the signature of the function to
                    // call is supported, it is guaranteed that the type of this return value is
                    // supported too.
                    return_value: Ok(val),
                })
            }
            corooteen::RunOut::Interrupted(FromCoroutine::Interrupt {
                function_index,
                parameters,
            }) => Ok(ExecOutcome::Interrupted {
                id: function_index,
                params: parameters,
            }),

            // `Init` must only be produced at initialization.
            corooteen::RunOut::Interrupted(FromCoroutine::Init) => unreachable!(),
            // `StartResult` only happens in response to a request.
            corooteen::RunOut::Interrupted(FromCoroutine::StartResult(_)) => unreachable!(),
            // `GetGlobalResponse` only happens in response to a request.
            corooteen::RunOut::Interrupted(FromCoroutine::GetGlobalResponse(_)) => unreachable!(),
        }
    }

    /// See [`super::VirtualMachine::memory_size`].
    pub fn memory_size(&self) -> u32 {
        let mem = match self.memory.as_ref() {
            Some(m) => m,
            None => return 0,
        };

        u32::try_from(mem.data_size()).unwrap()
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
                    Ok(&[][..])
                } else {
                    Err(OutOfBoundsError)
                }
            }
        };

        let start = usize::try_from(offset).map_err(|_| OutOfBoundsError)?;
        let end = start
            .checked_add(usize::try_from(size).map_err(|_| OutOfBoundsError)?)
            .ok_or(OutOfBoundsError)?;

        // Soundness: the documentation of wasmtime precisely explains what is safe or not.
        // Basically, we are safe as long as we are sure that we don't potentially grow the
        // buffer (which would invalidate the buffer pointer).
        unsafe {
            if end > mem.data_unchecked().len() {
                return Err(OutOfBoundsError);
            }

            Ok(&mem.data_unchecked()[start..end])
        }
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

        let start = usize::try_from(offset).map_err(|_| OutOfBoundsError)?;
        let end = start.checked_add(value.len()).ok_or(OutOfBoundsError)?;

        // Soundness: the documentation of wasmtime precisely explains what is safe or not.
        // Basically, we are safe as long as we are sure that we don't potentially grow the
        // buffer (which would invalidate the buffer pointer).
        unsafe {
            if end > mem.data_unchecked().len() {
                return Err(OutOfBoundsError);
            }

            if !value.is_empty() {
                mem.data_unchecked_mut()[start..end].copy_from_slice(value);
            }
        }

        Ok(())
    }

    /// See [`super::VirtualMachine::into_prototype`].
    pub fn into_prototype(self) -> JitPrototype {
        // TODO: how do we handle if the coroutine was within a host function?

        // TODO: necessary?
        /*// Zero-ing the memory.
        if let Some(memory) = &self.memory {
            // Soundness: the documentation of wasmtime precisely explains what is safe or not.
            // Basically, we are safe as long as we are sure that we don't potentially grow the
            // buffer (which would invalidate the buffer pointer).
            unsafe {
                for byte in memory.data_unchecked_mut() {
                    *byte = 0;
                }
            }
        }*/

        JitPrototype {
            coroutine: self.coroutine,
            memory: self.memory,
            indirect_table: self.indirect_table,
        }
    }
}

// The fields related to `wasmtime` do not implement `Send` because they use `std::rc::Rc`. `Rc`
// does not implement `Send` because incrementing/decrementing the reference counter from
// multiple threads simultaneously would be racy. It is however perfectly sound to move all the
// instances of `Rc`s at once between threads, which is what we're doing here.
//
// This importantly means that we should never return a `Rc` (even by reference) across the API
// boundary.
// TODO: really annoying to have to use unsafe code
unsafe impl Send for Jit {}

impl fmt::Debug for Jit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Jit").finish()
    }
}

/// Type that can be given to the coroutine.
enum ToCoroutine {
    /// Start execution of the given function. Answered with [`FromCoroutine::Init`].
    Start(String, Vec<WasmValue>),
    /// Resume execution after [`FromCoroutine::Interrupt`].
    Resume(Option<WasmValue>),
    /// Return the value of the given global with a [`FromCoroutine::GetGlobalResponse`].
    GetGlobal(String),
}

/// Type yielded by the coroutine.
enum FromCoroutine {
    /// Reports how well the initialization went. Sent as part of the first interrupt.
    Init,
    /// Reponse to [`ToCoroutine::Start`].
    StartResult(Result<(), StartErr>),
    /// Execution of the Wasm code has been interrupted by a call.
    Interrupt {
        /// Index of the function, to put in [`ExecOutcome::Interrupted::id`].
        function_index: usize,
        /// Parameters of the function.
        parameters: Vec<WasmValue>,
    },
    /// Response to a [`ToCoroutine::GetGlobal`].
    GetGlobalResponse(Result<u32, GlobalValueErr>),
    /// Executing the function is finished.
    Done(Result<Option<WasmValue>, String>),
}
