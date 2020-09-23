// Copyright (C) 2019-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::rc::Rc;
use core::{cell::Cell, marker::PhantomData, pin::Pin};

// Documentation about the x86_64 ABI here: https://github.com/hjl-tools/x86-psABI/wiki/X86-psABI

// TODO: make it works on 32bits
// TODO: the UnwindSafe trait should be enforced but isn't available in core
// TODO: require Send trait for the closure? it's a bit tricky, need to look into details
// TODO: will leak heap-allocated stuff if Dropped before it's finished

/// Prototype for a [`Coroutine`].
pub struct CoroutineBuilder<TInt, TRes> {
    stack_size: usize,
    state: Rc<Shared<TInt, TRes>>,
}

/// Runnable coroutine.
pub struct Coroutine<TExec, TInt, TRes> {
    /// We use a `u128` because the stack must be 16-bytes-aligned.
    _stack: Pin<Box<[u128]>>,
    /// State shared between the coroutine and the interrupters.
    state: Rc<Shared<TInt, TRes>>,
    /// True if the coroutine has been run at least once.
    has_run_once: bool,
    /// True if the coroutine has finished. Trying to resume running will panic.
    has_finished: bool,
    marker: PhantomData<TExec>,
}

/// Object whose intent is to be stored in the closure and that is capable of interrupting
/// execution.
pub struct Interrupter<TInt, TRes> {
    state: Rc<Shared<TInt, TRes>>,
}

struct Shared<TInt, TRes> {
    /// Value to put in `rsp` in order to resume the coroutine.
    ///
    /// Must point somewhere within [`Coroutine::stack`]. Will contain null before initialization.
    ///
    /// On initialization, must point to a memory location that contains the second parameter of
    /// `start_call`.
    /// Afterwards, must point to a memory location that contains the value 0 then
    /// the saved value of `rbp`, then the value of `rip` to return to.
    ///
    /// In order to resume the coroutine, set `rsp` to this value then pop all the registers.
    coroutine_stack_pointer: Cell<usize>,

    /// Stack pointer of the caller. Only valid if we are within the coroutine.
    caller_stack_pointer: Cell<usize>,

    /// Where to write the return value.
    potential_return_value_ptr: Cell<usize>,
    /// Storage where to write the value yielded to outside the coroutine before jumping out,
    /// or left to `None` if the coroutine has terminated.
    interrupt_val: Cell<Option<TInt>>,

    /// Storage where to write the value yielded back *to* the coroutine before resuming
    /// execution.
    resume_value: Cell<Option<TRes>>,
}

impl<TInt, TRes> Default for CoroutineBuilder<TInt, TRes> {
    fn default() -> Self {
        Self::new()
    }
}

impl<TInt, TRes> CoroutineBuilder<TInt, TRes> {
    /// Starts a builder.
    pub fn new() -> Self {
        CoroutineBuilder {
            // TODO: no stack protection :(
            stack_size: 4 * 1024 * 1024,
            state: Rc::new(Shared {
                coroutine_stack_pointer: Cell::new(0),
                caller_stack_pointer: Cell::new(0),
                potential_return_value_ptr: Cell::new(0),
                interrupt_val: Cell::new(None),
                resume_value: Cell::new(None),
            }),
        }
    }

    /// Creates a new [`Interrupter`] to store within the closure1.
    // TODO: it's super unsafe to use an interrupter with a different closure than the one passed
    // to build
    pub fn interrupter(&self) -> Interrupter<TInt, TRes> {
        Interrupter {
            state: self.state.clone(),
        }
    }

    /// Builds the coroutine.
    pub fn build<TRet, TExec: FnOnce() -> TRet>(
        self,
        to_exec: TExec,
    ) -> Coroutine<TExec, TInt, TRes> {
        let to_actually_exec = Box::into_raw({
            let state = self.state.clone();
            // TODO: panic safety handling here
            let closure = Box::new(move |caller_stack_pointer| {
                state.caller_stack_pointer.set(caller_stack_pointer);
                let ret_value = to_exec();
                let ptr =
                    unsafe { &mut *(state.potential_return_value_ptr.get() as *mut Option<TRet>) };
                assert!(ptr.is_none());
                *ptr = Some(ret_value);
                unsafe {
                    coroutine_switch_stack(state.caller_stack_pointer.get());
                    core::hint::unreachable_unchecked()
                }
            }) as Box<dyn FnOnce(usize)>;
            Box::new(closure) as Box<Box<dyn FnOnce(usize)>>
        });

        let mut stack = Pin::new(unsafe {
            let stack = Box::new_uninit_slice(1 + (self.stack_size.checked_sub(1).unwrap() / 16));
            stack.assume_init()
        });

        unsafe {
            let stack_top = (stack.as_mut_ptr() as *mut u64)
                .add(self.stack_size / 8)
                .sub(1);
            debug_assert!(!to_actually_exec.is_null());
            stack_top.add(0).write(to_actually_exec as usize as u64);

            self.state.coroutine_stack_pointer.set(stack_top as usize);
        }

        Coroutine {
            _stack: stack,
            state: self.state.clone(),
            has_run_once: false,
            has_finished: false,
            marker: PhantomData,
        }
    }
}

/// Return value of [`Coroutine::run`].
pub enum RunOut<TRet, TInt> {
    /// The coroutine has finished. Contains the return value of the closure.
    Finished(TRet),
    /// The coroutine has called [`Interrupter::interrupt`]
    Interrupted(TInt),
}

impl<TExec: FnOnce() -> TRet, TRet, TInt, TRes> Coroutine<TExec, TInt, TRes> {
    /// Returns true if running the closure has produced a [`RunOut::Finished`] earlier.
    pub fn is_finished(&self) -> bool {
        self.has_finished
    }

    /// Runs the coroutine until it finishes or is interrupted.
    ///
    /// `resume` must be `None` the first time a coroutine is run, then must be `Some` with the
    /// value to reinject back as the return value of [`Interrupter::interrupt`].
    ///
    /// # Panic
    ///
    /// Panics if [`RunOut::Finished`] has been returned earlier.
    /// Panics if `None` is passed and it is not the first time the coroutine is being run.
    /// Panics if `Some` is passed and it is the first time the coroutine is being run.
    ///
    pub fn run(&mut self, resume: Option<TRes>) -> RunOut<TRet, TInt> {
        assert!(!self.has_finished);

        if !self.has_run_once {
            assert!(resume.is_none());
            self.has_run_once = true;
        } else {
            assert!(resume.is_some());
        }

        // Store the resume value for the coroutine to pick up.
        debug_assert!(self.state.resume_value.take().is_none());
        self.state.resume_value.set(resume);

        // We allocate some space where the coroutine is allowed to put its return value, and put
        // a pointer to this space in `self.state`.
        let mut potential_return_value = None::<TRet>;
        self.state
            .potential_return_value_ptr
            .set(&mut potential_return_value as *mut _ as usize);

        // Doing a jump to the coroutine, which will then jump back here once it interrupts or
        // finishes.
        let new_stack_ptr =
            unsafe { coroutine_switch_stack(self.state.coroutine_stack_pointer.get()) };
        self.state.coroutine_stack_pointer.set(new_stack_ptr);
        debug_assert!(self.state.resume_value.take().is_none());

        // We determine whether the function has ended or is simply interrupted based on the
        // content of `self.state`.
        if let Some(interrupted) = self.state.interrupt_val.take() {
            debug_assert!(potential_return_value.take().is_none());
            RunOut::Interrupted(interrupted)
        } else {
            self.has_finished = true;
            RunOut::Finished(potential_return_value.take().unwrap())
        }
    }
}

impl<TInt, TRes> Interrupter<TInt, TRes> {
    /// Interrupts the current execution flow and jumps back to the [`Coroutine::run`] function,
    /// which will then return a [`RunOut::Interrupted`] containing the value passed as parameter.
    pub fn interrupt(&self, val: TInt) -> TRes {
        debug_assert!(self.state.interrupt_val.take().is_none());
        self.state.interrupt_val.set(Some(val));

        let new_caller_ptr =
            unsafe { coroutine_switch_stack(self.state.caller_stack_pointer.get()) };
        self.state.caller_stack_pointer.set(new_caller_ptr);

        self.state.resume_value.take().unwrap()
    }
}

impl<TInt, TRes> Clone for Interrupter<TInt, TRes> {
    fn clone(&self) -> Self {
        Interrupter {
            state: self.state.clone(),
        }
    }
}

/// Function whose role is to bootstrap the coroutine.
///
/// `caller_stack_pointer` is the value produced by [`coroutine_switch_stack`], and [`to_exec`]
/// is a pointer to the closure to execute. The closure must never return.
// TODO: turn `Box<dyn FnOnce(usize)>` into `Box<dyn FnOnce(usize) -> !>` when `!` is stable
extern "C" fn start_call(caller_stack_pointer: usize, to_exec: usize) {
    unsafe {
        let to_exec: Box<Box<dyn FnOnce(usize)>> =
            Box::from_raw(to_exec as *mut Box<dyn FnOnce(usize)>);
        (*to_exec)(caller_stack_pointer);
        core::hint::unreachable_unchecked()
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn coroutine_switch_stack(stack: usize) -> usize {
    let stack_out;
    asm!(r#"
        // Push rip on the stack.
        call 1f

        // At label `2` we will jump back here, which is why we need to jump to `3`.
        jmp 3f

    1:  push rbp

        // As seen later, after a stack change we always check the value at the top of the stack
        // to determine whether `start_call` must be called. Push a 0 to signify that no.
        push 0

        // Keep the current stack for later.
        mov rax, rsp

        // Set the stack to the request one.
        mov rsp, rdi

        // At the top of this new stack there must always be the value of rsi
        pop rsi

        // This value of rsi must always be 0 unless it is the entrance of the coroutine, in
        // which case rsi is the parameter to pass to `start_call`. We call `start_call` with the
        // appropriate parameters.
        cmp rsi, 0
        je 2f
        mov rdi, rax
        call {}
        int 3

        // If we reach here, it means that we are about to return to a code that has indeed called
        // `coroutine_switch_stack` earlier.
        // Pop rip.
        // Since we will eventually jump to label `3` below it may seem like we could just do
        // `add rsp, 8`, but since this inline assembly can exist in multiple versions in the final
        // binary, it is not guaranteed that the label `3` below is the same as the one we will
        // end up jumping to after the `ret`.
    2:  pop rbp
        ret

        3: nop
    "#,
        sym start_call,
        inout("rdi") stack => _, out("rsi") _,
        out("rax") stack_out, out("rcx") _, out("rdx") _, out("rbx") _,
        out("r8") _, out("r9") _, out("r10") _, out("r11") _,
        out("r12") _, out("r13") _, out("r14") _, out("r15") _,
        out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
        out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
        out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
        out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _
        // TODO: do we have all registers?
    );

    stack_out
}

#[cfg(test)]
mod tests {
    use super::{CoroutineBuilder, RunOut};

    #[test]
    fn basic_works() {
        let mut coroutine = CoroutineBuilder::<(), ()>::new().build(|| 12);
        match coroutine.run(None) {
            RunOut::Finished(12) => {}
            _ => panic!(),
        }
    }

    #[test]
    fn basic_interrupter() {
        let builder = CoroutineBuilder::new();
        let interrupter = builder.interrupter();
        let mut coroutine = builder.build(|| {
            let val = interrupter.interrupt(format!("hello world {}", 53));
            val + 12
        });

        match coroutine.run(None) {
            RunOut::Interrupted(val) => assert_eq!(val, "hello world 53"),
            _ => panic!(),
        }

        match coroutine.run(Some(5)) {
            RunOut::Finished(17) => {}
            _ => panic!(),
        }
    }

    #[test]
    fn many_interruptions() {
        let builder = CoroutineBuilder::new();
        let interrupter = builder.interrupter();
        let mut coroutine = builder.build(|| {
            let mut val = 0;
            for _ in 0..1000 {
                val = interrupter.interrupt(format!("hello! {}", val));
            }
            val
        });

        let mut val = None;
        loop {
            match coroutine.run(val) {
                RunOut::Interrupted(v) => {
                    assert_eq!(v, format!("hello! {}", val.unwrap_or(0)));
                    val = Some(val.unwrap_or(0) + 1);
                }
                RunOut::Finished(v) => {
                    assert_eq!(v, val.unwrap());
                    break;
                }
            }
        }
    }
}
