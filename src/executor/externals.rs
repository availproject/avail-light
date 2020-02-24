use super::{allocator, vm};
use core::convert::TryFrom as _;

/// WASM virtual machine specific to the Substrate/Polkadot Runtime Environment.
///
/// Contrary to [`VirtualMachine`](super::vm::VirtualMachine), this code is not just a generic
/// WASM virtual machine, but is aware of the runtime environment. The external functions that
/// the WASM code calls are automatically resolved and either handled or notified to the user of
/// this module.
pub struct ExternalsVm {
    /// Inner lower-level virtual machine.
    vm: vm::VirtualMachine,
    state: StateInner,

    /// List of functions that the WASM code imports.
    ///
    /// The keys of this `Vec` (i.e. the `usize` indices) have been passed to the virtual machine
    /// executor. Whenever the WASM code invokes an external function, we obtain its index, and
    /// look within this `Vec` to know what to do.
    registered_functions: Vec<RegisteredFunction>,

    /// Memory allocator in order to answer the calls to `malloc` and `free`.
    allocator: allocator::FreeingBumpHeapAllocator,
}

enum RegisteredFunction {
    /// The external function performs some immediate action and returns.
    Immediate {
        /// How to execute this function.
        implementation: Box<dyn FnMut(&[vm::RuntimeValue], &mut vm::VirtualMachine, &mut allocator::FreeingBumpHeapAllocator) -> Option<vm::RuntimeValue> + Send>,
    },
    /// Function isn't known.
    Unresolved {
        /// Name of the function.
        name: String,
    },
}

enum StateInner {
    Ready(Option<vm::RuntimeValue>),
    Finished(Vec<u8>),
}

/// State of a [`ExternalVm`]. Mutably borrows the virtual machine, thereby ensuring that the
/// state can't change.
pub enum State<'a> {
    ReadyToRun(StateReadyToRun<'a>),
    Finished(&'a [u8]),
    ExternalHashingKeccak256(StateWaitExternalResolve<'a, [u8]>),
}

pub struct StateReadyToRun<'a> {
    inner: &'a mut ExternalsVm,
}

pub struct StateWaitExternalResolve<'a, T: ?Sized> {
    inner: &'a mut ExternalsVm,
    resume: Box<dyn FnOnce(&mut ExternalsVm, &T) -> Option<vm::RuntimeValue>>,
}

impl ExternalsVm {
    /// Creates a new state machine from the given module that executes the given function.
    ///
    /// The `data` parameter is the SCALE-encoded data to inject into the runtime.
    pub fn new(
        module: &vm::WasmBlob,
        function_name: &str,
        data: &[u8],
    ) -> Result<Self, vm::NewErr> {
        // TODO: write params in memory
        assert!(data.is_empty(), "not implemented");

        let mut registered_functions = Vec::new();
        let vm = vm::VirtualMachine::new(
            module,
            function_name,
            &[vm::RuntimeValue::I32(0), vm::RuntimeValue::I32(0)],
            |mod_name, f_name, signature| {
                if mod_name != "env" {
                    return Err(());
                }

                let id = registered_functions.len();
                registered_functions.push(get_function(f_name));
                Ok(id)
            },
        )?;

        // In the runtime environment, WASM blobs must export a global symbol named `__heap_base`
        // indicating where the memory allocator is allowed to allocate memory.
        let heap_base = vm.global_value("__heap_base").unwrap(); // TODO: don't unwrap

        Ok(ExternalsVm {
            vm,
            state: StateInner::Ready(None),
            registered_functions,
            allocator: allocator::FreeingBumpHeapAllocator::new(heap_base),
        })
    }

    pub fn state(&mut self) -> State {
        // We have to do that in two steps in order to satisfy the borrow checker.
        match self.state {
            StateInner::Ready(_) => return State::ReadyToRun(StateReadyToRun { inner: self }),
            StateInner::Finished(_) => {},
        }
        match &self.state {
            StateInner::Ready(_) => unreachable!(),
            StateInner::Finished(data) => State::Finished(data),
        }
    }
}

impl<'a> From<StateReadyToRun<'a>> for State<'a> {
    fn from(state: StateReadyToRun<'a>) -> State<'a> {
        State::ReadyToRun(state)
    }
}

impl<'a> StateReadyToRun<'a> {
    pub fn run(self) -> State<'a> {
        loop {
            let resume_value = match &mut self.inner.state {
                StateInner::Ready(val) => val.take(),
                _ => unreachable!(),
            };

            match self.inner.vm.run(resume_value) {
                Ok(vm::ExecOutcome::Finished {
                    return_value: Ok(Some(vm::RuntimeValue::I64(ret))),
                }) => {
                    // According to the runtime environment specifies, the return vlaue is two
                    // consecutive I32s representing the length and size of the SCALE-encoded return
                    // value.
                    let ret = match u64::try_from(ret) {
                        Ok(r) => r,
                        Err(_) => panic!(), // TODO: don't panic
                    };

                    let ret_len = u32::try_from(ret >> 32).unwrap();
                    let ret_ptr = u32::try_from(ret & 0xffffffff).unwrap();
                    let ret_data = self.inner.vm.read_memory(ret_ptr, ret_len).unwrap();// TODO: don't unwrap
                    self.inner.state = StateInner::Finished(ret_data);
                    break;
                }
                Ok(vm::ExecOutcome::Interrupted { id, params }) => {
                    match self.inner.registered_functions.get_mut(id) {
                        Some(RegisteredFunction::Unresolved { name }) => {
                            panic!("Unresolved function called: {:?}", name) // TODO: don't panic
                        }
                        Some(RegisteredFunction::Immediate { implementation }) => {
                            let ret_val = implementation(&params, &mut self.inner.vm, &mut self.inner.allocator);
                            self.inner.state = StateInner::Ready(ret_val);
                        }

                        // Internal error. We have been passed back an `id` that we didn't return
                        // during the imports resolution.
                        None => panic!(),
                    }
                }
                e => unimplemented!("{:?}", e),
            }
        }

        self.inner.state()
    }
}

impl<'a, T> StateWaitExternalResolve<'a, T> {
    pub fn finish_call(self, resolve: &T) -> StateReadyToRun<'a> {
        self.inner.state = StateInner::Ready((self.resume)(self.inner, resolve));
        StateReadyToRun { inner: self.inner }
    }
}

fn get_function(name: &str) -> RegisteredFunction {
    match name {
        "ext_allocator_malloc_version_1" => {
            RegisteredFunction::Immediate { implementation: Box::new(move |params, vm, alloc| {
                let param = match params[0] {
                    vm::RuntimeValue::I32(v) => u32::try_from(v).unwrap(), // TODO: don't unwrap
                    _ => panic!(),  // TODO:
                };

                let ret = alloc.allocate(&mut MemAccess(vm), param).unwrap();  // TODO: don't unwrap
                Some(vm::RuntimeValue::I32(i32::try_from(ret).unwrap()))        // TODO: don't unwrap
            })}
        },
        "ext_allocator_free_version_1" => {
            RegisteredFunction::Immediate { implementation: Box::new(move |params, vm, alloc| {
                let param = match params[0] {
                    vm::RuntimeValue::I32(v) => u32::try_from(v).unwrap(), // TODO: don't unwrap
                    _ => panic!(),  // TODO:
                };

                alloc.deallocate(&mut MemAccess(vm), param).unwrap();  // TODO: don't unwrap
                None
            })}
        },
        _ => RegisteredFunction::Unresolved {
            name: name.to_owned(),
        },
    }
}

struct MemAccess<'a>(&'a mut vm::VirtualMachine);
impl<'a> allocator::Memory for MemAccess<'a> {
    fn read_le_u64(&self, ptr: u32) -> Result<u64, allocator::Error> {
        let bytes = self.0.read_memory(ptr, 8).unwrap();        // TODO: convert error
        Ok(u64::from_le_bytes(<[u8; 8]>::try_from(&bytes[..]).unwrap()))
    }

    fn write_le_u64(&mut self, ptr: u32, val: u64) -> Result<(), allocator::Error> {
        let bytes = val.to_le_bytes();
        self.0.write_memory(ptr, &bytes).unwrap(); // TODO: convert error instead
		Ok(())
    }

    fn size(&self) -> u32 {
        self.0.memory_size()
    }
}

#[cfg(test)]
mod tests {
    use super::ExternalsVm;

    #[test]
    fn is_send() {
        fn req<T: Send>() {}
        req::<ExternalsVm>();
    }
}
