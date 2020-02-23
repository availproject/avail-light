use alloc::{borrow::ToOwned as _, boxed::Box, format, vec::Vec};
use core::{
    cell::RefCell,
    convert::{TryFrom, TryInto as _},
    fmt,
};

/// WASM virtual machine that executes a specific function.
///
/// # Usage
///
/// - Create an instance of [`VirtualMachine`] with [`VirtualMachine::new`]. This operation only
/// initializes the machine but doesn't run it. As parameter, you must pass a list of external
/// functions that are available to the code running in the virtual machine.
///
/// - Call [`VirtualMachine::run`], passing `None` as parameter. This runs the WASM virtual
/// machine until either function finishes or calls an external function.
///
/// - If [`VirtualMachine::run`] returns [`ExecOutcome::Finished`], then it is forbidden to call
/// [`VirtualMachine::run`].
///
/// - If [`VirtualMachine::run`] returns [`ExecOutcome::Interrupted`], then you must later call
/// [`VirtualMachine::run`] again, passing the return value of the external function.
///
pub struct VirtualMachine {
    /// Original module, with resolved imports.
    module: wasmi::ModuleRef,

    /// Memory of the module instantiation.
    ///
    /// Right now we only support one unique `Memory` object per process. This is it.
    /// Contains `None` if the process doesn't export any memory object, which means it doesn't use
    /// any memory.
    memory: Option<wasmi::MemoryRef>,

    /// Table of the indirect function calls.
    ///
    /// In WASM, function pointers are in reality indices in a table called
    /// `__indirect_function_table`. This is this table, if it exists.
    indirect_table: Option<wasmi::TableRef>,

    /// Execution context of this virtual machine. This notably holds the program counter, state
    /// of the stack, and so on.
    ///
    /// This field is an `Option` because we need to be able to temporarily extract it. It must
    /// always be `Some`.
    execution: Option<wasmi::FuncInvocation<'static>>,

    /// If false, then one must call `execution.start_execution()` instead of `resume_execution()`.
    /// This is a particularity of the WASM interpreter that we don't want to expose in our API.
    interrupted: bool,

    /// If true, the state machine is in a poisoned state and cannot run any code anymore.
    is_poisoned: bool,
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
        return_value: Result<Option<wasmi::RuntimeValue>, ()>,
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
        params: Vec<wasmi::RuntimeValue>,
    },
}

/// WASM blob known to be valid.
// Note: this struct exists in order to hide wasmi as an implementation detail.
pub struct WasmBlob {
    inner: wasmi::Module,
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

/// Error that can happen when resuming the execution of a function.
#[derive(Debug)]
pub enum RunErr {
    /// The state machine is poisoned.
    Poisoned,
    /// Passed a wrong value back.
    BadValueTy {
        /// Type of the value that was expected.
        expected: Option<wasmi::ValueType>,
        /// Type of the value that was actually passed.
        obtained: Option<wasmi::ValueType>,
    },
}

impl VirtualMachine {
    /// Creates a new state machine from the given module that executes the given function.
    ///
    /// The closure is called for each import that the module has. It must assign a number to each
    /// import, or return an error if the import can't be resolved. When the VM calls one of these
    /// functions, this number will be returned back in order for the user to know how to handle
    /// the call.
    // TODO: don't expose wasmi in API
    pub fn new(
        module: &WasmBlob,
        function_name: &str,
        params: &[wasmi::RuntimeValue],
        mut symbols: impl FnMut(&str, &str, &wasmi::Signature) -> Result<usize, ()>,
    ) -> Result<Self, NewErr> {
        struct ImportResolve<'a>(
            RefCell<&'a mut dyn FnMut(&str, &str, &wasmi::Signature) -> Result<usize, ()>>,
        );
        impl<'a> wasmi::ImportResolver for ImportResolve<'a> {
            fn resolve_func(
                &self,
                module_name: &str,
                field_name: &str,
                signature: &wasmi::Signature,
            ) -> Result<wasmi::FuncRef, wasmi::Error> {
                let closure = &mut **self.0.borrow_mut();
                let index = match closure(module_name, field_name, signature) {
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
                _field_name: &str,
                _memory_type: &wasmi::MemoryDescriptor,
            ) -> Result<wasmi::MemoryRef, wasmi::Error> {
                Err(wasmi::Error::Instantiation(
                    "Importing memory is not supported yet".to_owned(),
                ))
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

        let not_started =
            wasmi::ModuleInstance::new(&module.inner, &ImportResolve(RefCell::new(&mut symbols)))
                .map_err(NewErr::Interpreter)?;
        // TODO: explain `assert_no_start`
        let module = not_started.assert_no_start();

        let memory = if let Some(mem) = module.export_by_name("memory") {
            if let Some(mem) = mem.as_memory() {
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

        let execution = match module.export_by_name(function_name) {
            Some(wasmi::ExternVal::Func(f)) => {
                match wasmi::FuncInstance::invoke_resumable(&f, params.to_vec()) {
                    Ok(e) => e,
                    Err(err) => unreachable!("{:?}", err),
                }
            }
            None => return Err(NewErr::FunctionNotFound),
            _ => return Err(NewErr::NotAFunction),
        };

        Ok(VirtualMachine {
            module,
            memory,
            execution: Some(execution),
            interrupted: false,
            indirect_table,
            is_poisoned: false,
        })
    }

    /// Returns true if the state machine is in a poisoned state and cannot run anymore.
    pub fn is_poisoned(&self) -> bool {
        self.is_poisoned
    }

    /// Starts or continues execution of the virtual machine.
    ///
    /// If this is the first call you call [`run`](VirtualMachine::run), then you must pass
    /// a value of `None`.
    /// If, however, you call this function after a previous call to [`run`](VirtualMachine::run)
    /// that was interrupted by an external function call, then you must pass back the outcome of
    /// that call.
    pub fn run(&mut self, value: Option<wasmi::RuntimeValue>) -> Result<ExecOutcome, RunErr> {
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

        let result = if self.interrupted {
            let expected_ty = execution.resumable_value_type();
            let obtained_ty = value.as_ref().map(|v| v.value_type());
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
                    obtained: value.as_ref().map(|v| v.value_type()),
                });
            }
            self.interrupted = true;
            execution.start_execution(&mut DummyExternals)
        };

        match result {
            Ok(return_value) => {
                self.is_poisoned = true;
                Ok(ExecOutcome::Finished {
                    return_value: Ok(return_value),
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
                    params: interrupt.args.clone(),
                })
            }
            Err(wasmi::ResumableError::Trap(_)) => {
                self.is_poisoned = true;
                Ok(ExecOutcome::Finished {
                    return_value: Err(()),
                })
            }
        }
    }

    /// Copies the given memory range into a `Vec<u8>`.
    ///
    /// Returns an error if the range is invalid or out of range.
    pub fn read_memory(&self, offset: u32, size: u32) -> Result<Vec<u8>, ()> {
        let mem = match self.memory.as_ref() {
            Some(m) => m,
            None => unreachable!(),
        };

        mem.get(offset, size.try_into().map_err(|_| ())?)
            .map_err(|_| ())
    }

    /// Write the data at the given memory location.
    ///
    /// Returns an error if the range is invalid or out of range.
    pub fn write_memory(&mut self, offset: u32, value: &[u8]) -> Result<(), ()> {
        let mem = match self.memory.as_ref() {
            Some(m) => m,
            None => unreachable!(),
        };

        mem.set(offset, value).map_err(|_| ())
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
unsafe impl Send for VirtualMachine {}

impl fmt::Debug for VirtualMachine {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("VirtualMachine").finish()
    }
}

impl WasmBlob {
    // TODO: better error type
    pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Result<Self, ()> {
        let inner = wasmi::Module::from_buffer(bytes.as_ref()).map_err(|_| ())?;
        inner.deny_floating_point().map_err(|_| ())?;
        Ok(WasmBlob { inner })
    }
}

impl<'a> TryFrom<&'a [u8]> for WasmBlob {
    type Error = (); // TODO: better error type

    fn try_from(bytes: &'a [u8]) -> Result<Self, Self::Error> {
        WasmBlob::from_bytes(bytes)
    }
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

#[cfg(test)]
mod tests {
    // TODO:
}
