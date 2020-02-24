use super::vm;
use core::convert::TryFrom as _;

/// WASM virtual machine.
///
/// > **Note**: Contrary to [`VirtualMachine`](super::vm::VirtualMachine), this code is aware of
/// >           the runtime environment. The external functions that the WASM code calls are
/// >           automatically resolved and parsed into high-level structs.
pub struct ExternalsVm {
    vm: vm::VirtualMachine,
    state: StateInner,
    functions: hashbrown::HashMap<usize, String, fnv::FnvBuildHasher>,
}

enum StateInner {
    Ready(Option<vm::RuntimeValue>),
}

/// State of a [`ExternalVm`]. Mutably borrows the virtual machine, thereby ensuring that the
/// state can't change.
pub enum State<'a> {
    ReadyToRun(StateReadyToRun<'a>),
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
    /// The `data` parameter is the style-encoded data to inject into the runtime.
    pub fn new(
        module: &vm::WasmBlob,
        function_name: &str,
        data: &[u8],
    ) -> Result<Self, vm::NewErr> {
        // TODO: write params in memory
        assert!(data.is_empty(), "not implemented");
        let mut functions = hashbrown::HashMap::default();
        let mut num = 0;
        let vm = vm::VirtualMachine::new(module, function_name, &[vm::RuntimeValue::I32(0), vm::RuntimeValue::I32(0)], |mod_name, f_name, signature| {
            if mod_name != "env" {
                return Err(());
            }

            functions.insert(num, f_name.to_string());
            let n = num;
            num += 1;
            Ok(n)
        })?;

        Ok(ExternalsVm {
            vm,
            state: StateInner::Ready(None),
            functions,
        })
    }

    pub fn state(&mut self) -> State {
        match &self.state {
            StateInner::Ready(_) => {
                State::ReadyToRun(StateReadyToRun {
                    inner: self,
                })
            }
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
        let resume_value = match &mut self.inner.state {
            StateInner::Ready(val) => val.take(),
            _ => unreachable!(),
        };

        match self.inner.vm.run(resume_value) {
            Ok(vm::ExecOutcome::Finished { return_value: Ok(Some(vm::RuntimeValue::I64(ret))) }) => {
                // According to the runtime environment specifies, the return vlaue is two
                // consecutive I32s representing the length and size of the SCALE-encoded return
                // value.
                let ret = match u64::try_from(ret) {
                    Ok(r) => r,
                    Err(_) => panic!(),       // TODO: don't panic
                };

                let ret_len = u32::try_from(ret >> 32).unwrap();
                let ret_ptr = u32::try_from(ret & 0xffffffff).unwrap();
                let ret_data = self.inner.vm.read_memory(ret_ptr, ret_len).unwrap();  // TODO: don't unwrap
            },
            Ok(vm::ExecOutcome::Interrupted { id, params }) => {
                unimplemented!("{:?}", self.inner.functions.get(&id));
            },
            e => unimplemented!("{:?}", e),
        }

        unimplemented!()
    }
}

impl<'a, T> StateWaitExternalResolve<'a, T> {
    pub fn finish_call(self, resolve: &T) -> StateReadyToRun<'a> {
        self.inner.state = StateInner::Ready((self.resume)(self.inner, resolve));
        StateReadyToRun {
            inner: self.inner,
        }
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
