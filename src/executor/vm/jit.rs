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

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use core::{
    cell::RefCell,
    cmp,
    convert::TryFrom,
    fmt,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use futures::{
    future::{self, Future},
    task, FutureExt as _,
};

/// See [`super::Module`].
#[derive(Clone)]
pub struct Module {
    inner: wasmtime::Module,
}

impl Module {
    /// See [`super::Module::new`].
    pub fn new(module_bytes: impl AsRef<[u8]>) -> Result<Self, NewErr> {
        let mut config = wasmtime::Config::new();
        config.cranelift_nan_canonicalization(true);
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);
        let engine = wasmtime::Engine::new(&config);

        let inner = wasmtime::Module::from_binary(&engine, module_bytes.as_ref())
            .map_err(|err| NewErr::ModuleError(ModuleError(err.to_string())))?;

        Ok(Module { inner })
    }
}

/// See [`super::VirtualMachinePrototype`].
pub struct JitPrototype {
    /// Instanciated Wasm VM.
    instance: wasmtime::Instance,

    /// Shared between the "outside" and the external functions. See [`Shared`].
    shared: Rc<RefCell<Shared>>,

    /// Reference to the memory used by the module, if any.
    memory: Option<wasmtime::Memory>,

    /// Reference to the table of indirect functions, in case we need to access it.
    /// `None` if the module doesn't export such table.
    indirect_table: Option<wasmtime::Table>,
}

impl JitPrototype {
    /// See [`super::VirtualMachinePrototype::new`].
    pub fn new(
        module: &Module,
        heap_pages: HeapPages,
        mut symbols: impl FnMut(&str, &str, &Signature) -> Result<usize, ()>,
    ) -> Result<Self, NewErr> {
        let store = wasmtime::Store::new_async(&module.inner.engine());

        let mut imported_memory = None;
        let shared = Rc::new(RefCell::new(Shared {
            function_index: 0, // Dummy value.
            parameters: None,
            return_value: None,
            in_interrupted_waker: None,
        }));

        // Building the list of symbols that the Wasm VM is able to use.
        let imports = {
            let mut imports = Vec::with_capacity(module.inner.imports().len());
            for import in module.inner.imports() {
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

                        let shared = shared.clone();

                        imports.push(wasmtime::Extern::Func(wasmtime::Func::new_async(
                            &store,
                            f.clone(),
                            (),
                            move |_, (), params, ret_val| {
                                // This closure is executed whenever the Wasm VM calls a
                                // host function.

                                // Store the information about the function being called in
                                // `shared`.
                                let mut shared_lock = shared.borrow_mut();
                                shared_lock.function_index = function_index;
                                shared_lock.parameters = Some(
                                    params
                                        .iter()
                                        .map(TryFrom::try_from)
                                        .collect::<Result<_, _>>()
                                        .unwrap(),
                                );

                                // The first ever call to `Jit::run` for this instance sets
                                // `return_value` to `Some(None)`. This is not a legitimate
                                // return value, so it must be erased.
                                debug_assert!(matches!(
                                    shared_lock.return_value,
                                    None | Some(None)
                                ));
                                shared_lock.return_value = None;

                                // Return a future that is ready whenever `Shared::return_value`
                                // contains `Some`.
                                let shared = shared.clone();
                                Box::new(future::poll_fn(move |cx| {
                                    let mut shared = shared.borrow_mut();
                                    if let Some(returned) = shared.return_value.take() {
                                        if let Some(returned) = returned {
                                            assert_eq!(ret_val.len(), 1);
                                            ret_val[0] = From::from(returned);
                                        } else {
                                            assert!(ret_val.is_empty());
                                        }
                                        Poll::Ready(Ok(()))
                                    } else {
                                        shared.in_interrupted_waker = Some(cx.waker().clone());
                                        Poll::Pending
                                    }
                                }))
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

        // Note: we assume that `new_async` is instantaneous, which it seems to be. It's actually
        // unclear why this function is async at all, as all it is supposed to do in synchronous.
        // Future versions of `wasmtime` might break this assumption.
        let instance = wasmtime::Instance::new_async(&store, &module.inner, &imports)
            .now_or_never()
            .unwrap()
            .unwrap(); // TODO: don't unwrap

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

        Ok(JitPrototype {
            instance,
            shared,
            memory,
            indirect_table,
        })
    }

    /// See [`super::VirtualMachinePrototype::global_value`].
    pub fn global_value(&mut self, name: &str) -> Result<u32, GlobalValueErr> {
        match self.instance.get_export(name) {
            Some(wasmtime::Extern::Global(g)) => match g.get() {
                wasmtime::Val::I32(v) => Ok(u32::from_ne_bytes(v.to_ne_bytes())),
                _ => Err(GlobalValueErr::Invalid),
            },
            _ => Err(GlobalValueErr::NotFound),
        }
    }

    /// See [`super::VirtualMachinePrototype::start`].
    pub fn start(self, function_name: &str, params: &[WasmValue]) -> Result<Jit, (StartErr, Self)> {
        // Try to start executing `_start`.
        let start_function = match self.instance.get_export(function_name) {
            Some(export) => match export.into_func() {
                Some(f) => f,
                None => return Err((StartErr::NotAFunction, self)),
            },
            None => return Err((StartErr::FunctionNotFound, self)),
        };

        // Try to convert the signature of the function to call, in order to make sure
        // that the type of parameters and return value are supported.
        if Signature::try_from(&start_function.ty()).is_err() {
            return Err((StartErr::SignatureNotSupported, self));
        }

        // Now running the `start` function of the Wasm code.
        let params = params.iter().map(|v| (*v).into()).collect::<Vec<_>>();
        let function_call = Box::pin(async move {
            let result = start_function
                .call_async(&params)
                .await
                .map_err(|err| err.to_string())?; // The type of error is from the `anyhow` library. By using `to_string()` we avoid having to deal with it.

            // Execution resumes here when the Wasm code has gracefully finished.
            // The signature of the function has been chedk earlier, and as such it is
            // guaranteed that it has only one return value.
            assert!(result.len() == 0 || result.len() == 1);
            Ok(result.get(0).map(|v| TryFrom::try_from(v).unwrap()))
        });

        Ok(Jit {
            function_call: Some(function_call),
            instance: self.instance,
            shared: self.shared,
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

/// Data shared between the external API and the functions that `wasmtime` directly invokes.
///
/// The flow is as follows:
///
/// - `wasmtime` calls a function that has access to a `Rc<RefCell<Shared>>`.
/// - This function stores its information (`function_index` and `parameters`) in the `Shared`
/// and returns `Poll::Pending`.
/// - This `Pending` gets propagated to the body of [`Jit::run`], which was calling `wasmtime`.
/// [`Jit::run`] reads `function_index` and `parameters` to determine what happened.
/// - Later, the return value is stored in `return_value`, and execution is resumed.
/// - The function called by `wasmtime` reads `return_value` and returns `Poll::Ready`.
///
struct Shared {
    /// Index of the function currently being called.
    function_index: usize,

    /// Parameters of the function currently being called. Extracted when necessary.
    parameters: Option<Vec<WasmValue>>,

    /// `None` is no return value is available yet.
    /// Otherwise, `Some(value)` where `value` is the value to return to the Wasm code.
    return_value: Option<Option<WasmValue>>,

    /// Waker that `wasmtime` has passed to the future that is waiting for `return_value`.
    /// This value is most likely not very useful, because [`Jit::run`] always polls the outer
    /// future whenever the inner future is known to be ready.
    /// However, it would be completely legal for `wasmtime` to not poll the inner future if the
    /// waker that it has passed (the one stored here) wasn't waken up.
    /// This field therefore exists in order to future-proof against this possible optimization
    /// that `wasmtime` might perform in the future.
    in_interrupted_waker: Option<Waker>,
}

/// See [`super::VirtualMachine`].
pub struct Jit {
    /// Instanciated Wasm VM.
    instance: wasmtime::Instance,

    /// `Future` that drives the execution. Contains an invocation of
    /// `wasmtime::Func::call_async`.
    /// `None` if the execution has finished and future has returned `Poll::Ready` in the past.
    function_call: Option<Pin<Box<dyn Future<Output = Result<Option<WasmValue>, String>>>>>,

    /// Shared between the "outside" and the external functions. See [`Shared`].
    shared: Rc<RefCell<Shared>>,

    /// See [`JitPrototype::memory`].
    memory: Option<wasmtime::Memory>,

    /// See [`JitPrototype::indirect_table`].
    indirect_table: Option<wasmtime::Table>,
}

impl Jit {
    /// See [`super::VirtualMachine::run`].
    pub fn run(&mut self, value: Option<WasmValue>) -> Result<ExecOutcome, RunErr> {
        let function_call = match self.function_call.as_mut() {
            Some(f) => f,
            None => return Err(RunErr::Poisoned),
        };

        // TODO: check value type

        let mut shared_lock = self.shared.borrow_mut();
        shared_lock.return_value = Some(value);
        if let Some(waker) = shared_lock.in_interrupted_waker.take() {
            waker.wake();
        }
        drop(shared_lock);

        // Resume the coroutine execution.
        // The `Future` is polled with a no-op waker. We are in total control of when the
        // execution might be able to progress, hence the lack of need for a waker.
        match Future::poll(
            function_call.as_mut(),
            &mut Context::from_waker(task::noop_waker_ref()),
        ) {
            Poll::Ready(Ok(val)) => {
                self.function_call = None;
                Ok(ExecOutcome::Finished {
                    // Since we verify at initialization that the signature of the function to
                    // call is supported, it is guaranteed that the type of this return value is
                    // supported too.
                    return_value: Ok(val),
                })
            }
            Poll::Ready(Err(err)) => {
                self.function_call = None;
                Ok(ExecOutcome::Finished {
                    return_value: Err(Trap(err)),
                })
            }
            Poll::Pending => {
                let mut shared = self.shared.borrow_mut();
                Ok(ExecOutcome::Interrupted {
                    id: shared.function_index,
                    params: shared.parameters.take().unwrap(),
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
            instance: self.instance,
            shared: self.shared,
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
