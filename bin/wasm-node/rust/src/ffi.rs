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

// TODO: the quality of this module is sub-par

use core::{
    cmp::Ordering,
    convert::TryFrom as _,
    fmt,
    future::Future,
    iter, marker,
    ops::{Add, Sub},
    pin::Pin,
    ptr, slice,
    task::{Context, Poll, Waker},
    time::Duration,
};
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use std::{
    collections::VecDeque,
    sync::{atomic, Arc, Mutex},
    task,
};

pub mod bindings;

/// Stops execution, throwing a string exception with the given content.
pub(crate) fn throw(message: String) -> ! {
    unsafe {
        bindings::throw(
            u32::try_from(message.as_bytes().as_ptr() as usize).unwrap(),
            u32::try_from(message.as_bytes().len()).unwrap(),
        );

        // Even though this code is intended to only ever be compiled for Wasm, it might, for
        // various reasons, be compiled for the host platform as well. We use platform-specific
        // code to make sure that it compiles for all platforms.
        #[cfg(target_arch = "wasm32")]
        core::arch::wasm32::unreachable();
        #[cfg(not(target_arch = "wasm32"))]
        unreachable!();
    }
}

/// Returns the duration elapsed since the UNIX epoch, ignoring leap seconds.
pub(crate) fn unix_time() -> Duration {
    Duration::from_secs_f64(unsafe { bindings::unix_time_ms() } / 1000.0)
}

/// Spawn a background task that runs forever.
fn spawn_task(future: impl Future<Output = ()> + Send + 'static) {
    struct Waker {
        done: atomic::AtomicBool,
        wake_up_registered: atomic::AtomicBool,
        future: Mutex<Pin<Box<dyn Future<Output = ()> + Send>>>,
    }

    impl task::Wake for Waker {
        fn wake(self: Arc<Self>) {
            if self
                .wake_up_registered
                .swap(true, atomic::Ordering::Relaxed)
            {
                return;
            }

            start_timer_wrap(Duration::new(0, 0), move || {
                if self.done.load(atomic::Ordering::SeqCst) {
                    return;
                }

                let mut future = self.future.try_lock().unwrap();
                self.wake_up_registered
                    .store(false, atomic::Ordering::SeqCst);
                match Future::poll(
                    future.as_mut(),
                    &mut Context::from_waker(&task::Waker::from(self.clone())),
                ) {
                    Poll::Ready(()) => {
                        self.done.store(true, atomic::Ordering::SeqCst);
                    }
                    Poll::Pending => {}
                }
            })
        }
    }

    let waker = Arc::new(Waker {
        done: false.into(),
        wake_up_registered: false.into(),
        future: Mutex::new(Box::pin(future)),
    });

    task::Wake::wake(waker);
}

/// Uses the environment to invoke `closure` after `duration` has elapsed.
fn start_timer_wrap(duration: Duration, closure: impl FnOnce()) {
    let callback: Box<Box<dyn FnOnce()>> = Box::new(Box::new(closure));
    let timer_id = u32::try_from(Box::into_raw(callback) as usize).unwrap();
    let milliseconds = u64::try_from(duration.as_millis()).unwrap_or(u64::max_value());
    unsafe { bindings::start_timer(timer_id, (milliseconds as f64).ceil()) }
}

// TODO: cancel the timer if the `Delay` is destroyed? we create and destroy a lot of `Delay`s
pub struct Delay {
    rx: oneshot::Receiver<()>,
}

impl Delay {
    pub fn new(when: Duration) -> Self {
        let (tx, rx) = oneshot::channel();
        start_timer_wrap(when, move || {
            let _ = tx.send(());
        });
        Delay { rx }
    }
}

impl Future for Delay {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        Future::poll(Pin::new(&mut self.rx), cx).map(|v| v.unwrap())
    }
}

#[derive(Debug, Copy, Clone)]
pub struct Instant {
    /// Milliseconds.
    inner: f64,
}

impl PartialEq for Instant {
    fn eq(&self, other: &Instant) -> bool {
        self.inner == other.inner
    }
}

impl Eq for Instant {}

impl PartialOrd for Instant {
    fn partial_cmp(&self, other: &Instant) -> Option<Ordering> {
        self.inner.partial_cmp(&other.inner)
    }
}

impl Ord for Instant {
    fn cmp(&self, other: &Self) -> Ordering {
        self.inner.partial_cmp(&other.inner).unwrap()
    }
}

impl Instant {
    pub fn now() -> Instant {
        Instant {
            inner: unsafe { bindings::monotonic_clock_ms() },
        }
    }

    pub fn duration_since(&self, earlier: Instant) -> Duration {
        *self - earlier
    }

    pub fn elapsed(&self) -> Duration {
        Instant::now() - *self
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, other: Duration) -> Instant {
        let new_val = self.inner + other.as_millis() as f64;
        Instant {
            inner: new_val as f64,
        }
    }
}

impl Sub<Duration> for Instant {
    type Output = Instant;

    fn sub(self, other: Duration) -> Instant {
        let new_val = self.inner - other.as_millis() as f64;
        Instant {
            inner: new_val as f64,
        }
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, other: Instant) -> Duration {
        let ms = self.inner - other.inner;
        assert!(ms >= 0.0);
        Duration::from_millis(ms as u64)
    }
}

/// Implementation of [`log::Log`] that sends out logs to the FFI.
pub(crate) struct Logger;

impl log::Log for Logger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let target = record.target();
        let message = format!("{}", record.args());

        unsafe {
            bindings::log(
                record.level() as usize as u32,
                u32::try_from(target.as_bytes().as_ptr() as usize).unwrap(),
                u32::try_from(target.as_bytes().len()).unwrap(),
                u32::try_from(message.as_bytes().as_ptr() as usize).unwrap(),
                u32::try_from(message.as_bytes().len()).unwrap(),
            )
        }
    }

    fn flush(&self) {}
}

/// Connection connected to a target.
pub struct Connection {
    /// If `Some`, [`bindings::connection_close`] must be called. Set to a value after
    /// [`bindings::connection_new`] returns success.
    id: Option<u32>,
    /// True if [`bindings::connection_open`] has been called.
    open: bool,
    /// True if [`bindings::connection_closed`] has been called.
    closed: bool,
    /// List of messages received through [`bindings::connection_message`]. Must never contain
    /// empty messages.
    messages_queue: VecDeque<Box<[u8]>>,
    /// Position of the read cursor within the first element of [`Connection::messages_queue`].
    messages_queue_first_offset: usize,
    /// Waker to wake up whenever one of the fields above is modified.
    waker: Option<Waker>,
    /// Prevents the [`Connection`] from being unpinned.
    _pinned: marker::PhantomPinned,
}

impl Connection {
    /// Connects to the given URL. Returns a [`Connection`] on success.
    pub fn connect(url: &str) -> impl Future<Output = Result<Pin<Box<Self>>, ()>> {
        let mut pointer = Box::pin(Connection {
            id: None,
            open: false,
            closed: false,
            messages_queue: VecDeque::with_capacity(32),
            messages_queue_first_offset: 0,
            waker: None,
            _pinned: marker::PhantomPinned,
        });

        let id = u32::try_from(&*pointer as *const Connection as usize).unwrap();

        let ret_code = unsafe {
            bindings::connection_new(
                id,
                u32::try_from(url.as_bytes().as_ptr() as usize).unwrap(),
                u32::try_from(url.as_bytes().len()).unwrap(),
            )
        };

        async move {
            if ret_code != 0 {
                return Err(());
            }

            unsafe {
                Pin::get_unchecked_mut(pointer.as_mut()).id = Some(id);
            }

            future::poll_fn(|cx| {
                if pointer.closed || pointer.open {
                    return Poll::Ready(());
                }
                if pointer
                    .waker
                    .as_ref()
                    .map_or(true, |w| !cx.waker().will_wake(w))
                {
                    unsafe {
                        Pin::get_unchecked_mut(pointer.as_mut()).waker = Some(cx.waker().clone());
                    }
                }
                Poll::Pending
            })
            .await;

            if pointer.open {
                Ok(pointer)
            } else {
                debug_assert!(pointer.closed);
                Err(())
            }
        }
    }

    /// Returns a buffer containing data received on the connection.
    ///
    /// Never returns an empty buffer. If no data is available, this function waits until more
    /// data arrives.
    ///
    /// Returns `None` if the connection has been closed.
    pub async fn read_buffer<'a>(self: &'a mut Pin<Box<Self>>) -> Option<&'a [u8]> {
        future::poll_fn(|cx| {
            if !self.messages_queue.is_empty() || self.closed {
                return Poll::Ready(());
            }

            if self
                .waker
                .as_ref()
                .map_or(true, |w| !cx.waker().will_wake(w))
            {
                unsafe {
                    Pin::get_unchecked_mut(self.as_mut()).waker = Some(cx.waker().clone());
                }
            }
            Poll::Pending
        })
        .await;

        if let Some(buffer) = self.messages_queue.front() {
            debug_assert!(!buffer.is_empty());
            debug_assert!(self.messages_queue_first_offset < buffer.len());
            Some(&buffer[self.messages_queue_first_offset..])
        } else if self.closed {
            None
        } else {
            unreachable!()
        }
    }

    /// Advances the read cursor by the given amount of bytes. The first `bytes` will no longer
    /// be returned by [`Connection::read_buffer`] the next time it is called.
    ///
    /// # Panic
    ///
    /// Panics if `bytes` is larger than the size of the buffer returned by
    /// [`Connection::read_buffer`].
    ///
    pub fn advance_read_cursor(self: &mut Pin<Box<Self>>, bytes: usize) {
        let this = unsafe { Pin::get_unchecked_mut(self.as_mut()) };

        this.messages_queue_first_offset += bytes;

        if let Some(buffer) = this.messages_queue.front() {
            assert!(this.messages_queue_first_offset <= buffer.len());
            if this.messages_queue_first_offset == buffer.len() {
                this.messages_queue.pop_front();
                this.messages_queue_first_offset = 0;
            }
        } else {
            assert_eq!(bytes, 0);
        };
    }

    /// Queues the given buffer. For WebSocket connections, queues it as a binary frame.
    pub fn send(self: &mut Pin<Box<Self>>, data: &[u8]) {
        unsafe {
            let this = Pin::get_unchecked_mut(self.as_mut());

            // Connection might have been closed, but API user hasn't detected it yet.
            if this.closed {
                return;
            }

            bindings::connection_send(
                this.id.unwrap(),
                u32::try_from(data.as_ptr() as usize).unwrap(),
                u32::try_from(data.len()).unwrap(),
            );
        }
    }
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Connection")
            .field(self.id.as_ref().unwrap())
            .finish()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            unsafe {
                bindings::connection_close(id);
            }
        }
    }
}

fn alloc(len: u32) -> u32 {
    let len = usize::try_from(len).unwrap();
    let mut vec = Vec::<u8>::with_capacity(len);
    unsafe {
        vec.set_len(len);
    }
    let ptr: *mut [u8] = Box::into_raw(vec.into_boxed_slice());
    u32::try_from(ptr as *mut u8 as usize).unwrap()
}

fn init(chain_specs_pointers_ptr: u32, chain_specs_pointers_len: u32, max_log_level: u32) {
    let chain_specs_pointers_ptr = usize::try_from(chain_specs_pointers_ptr).unwrap();
    let chain_specs_pointers_len = usize::try_from(chain_specs_pointers_len).unwrap();

    let chain_specs_pointers: Box<[u8]> = unsafe {
        Box::from_raw(slice::from_raw_parts_mut(
            chain_specs_pointers_ptr as *mut u8,
            chain_specs_pointers_len,
        ))
    };

    assert_eq!(chain_specs_pointers.len() % 8, 0);
    let mut chain_specs = Vec::with_capacity(chain_specs_pointers.len() / 8);

    for chain_spec_index in 0..(chain_specs.capacity()) {
        let spec_pointer = {
            let val = <[u8; 4]>::try_from(
                &chain_specs_pointers[(chain_spec_index * 8)..(chain_spec_index * 8 + 4)],
            )
            .unwrap();
            usize::try_from(u32::from_le_bytes(val)).unwrap()
        };

        let spec_len = {
            let val = <[u8; 4]>::try_from(
                &chain_specs_pointers[(chain_spec_index * 8 + 4)..(chain_spec_index * 8 + 8)],
            )
            .unwrap();
            usize::try_from(u32::from_le_bytes(val)).unwrap()
        };

        let chain_spec: Box<[u8]> =
            unsafe { Box::from_raw(slice::from_raw_parts_mut(spec_pointer as *mut u8, spec_len)) };

        let chain_spec = String::from_utf8(Vec::from(chain_spec)).expect("non-utf8 chain spec");

        chain_specs.push(super::ChainConfig {
            specification: chain_spec,
            json_rpc_running: true,
        });
    }

    debug_assert_eq!(chain_specs.len(), chain_specs.capacity());

    let max_log_level = match max_log_level {
        0 => log::LevelFilter::Off,
        1 => log::LevelFilter::Error,
        2 => log::LevelFilter::Warn,
        3 => log::LevelFilter::Info,
        4 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };

    spawn_task(super::start_client(chain_specs.into_iter(), max_log_level));
}

pub(crate) enum JsonRpcMessage {
    Request {
        json_rpc_request: Box<[u8]>,
        chain_index: usize,
        user_data: u32,
    },
    UnsubscribeAll {
        user_data: u32,
    },
}

lazy_static::lazy_static! {
    static ref JSON_RPC_CHANNEL: (mpsc::UnboundedSender<JsonRpcMessage>, futures::lock::Mutex<mpsc::UnboundedReceiver<JsonRpcMessage>>) = {
        let (tx, rx) = mpsc::unbounded();
        (tx, futures::lock::Mutex::new(rx))
    };
}

fn json_rpc_send(ptr: u32, len: u32, chain_index: u32, user_data: u32) {
    let ptr = usize::try_from(ptr).unwrap();
    let len = usize::try_from(len).unwrap();
    let chain_index = usize::try_from(chain_index).unwrap();

    let json_rpc_request: Box<[u8]> =
        unsafe { Box::from_raw(slice::from_raw_parts_mut(ptr as *mut u8, len)) };
    let message = JsonRpcMessage::Request {
        json_rpc_request,
        chain_index,
        user_data,
    };
    JSON_RPC_CHANNEL.0.unbounded_send(message).unwrap();
}

fn json_rpc_unsubscribe_all(user_data: u32) {
    JSON_RPC_CHANNEL
        .0
        .unbounded_send(JsonRpcMessage::UnsubscribeAll { user_data })
        .unwrap();
}

/// Waits for the next JSON-RPC request coming from the JavaScript side.
// TODO: maybe tie the JSON-RPC system to a certain "client", instead of being global?
pub(crate) async fn next_json_rpc() -> JsonRpcMessage {
    let mut lock = JSON_RPC_CHANNEL.1.lock().await;
    lock.next().await.unwrap()
}

/// Emit a JSON-RPC response or subscription notification in destination to the JavaScript side.
// TODO: maybe tie the JSON-RPC system to a certain "client", instead of being global?
pub(crate) fn emit_json_rpc_response(rpc: &str, chain_index: usize, user_data: u32) {
    unsafe {
        bindings::json_rpc_respond(
            u32::try_from(rpc.as_bytes().as_ptr() as usize).unwrap(),
            u32::try_from(rpc.as_bytes().len()).unwrap(),
            u32::try_from(chain_index).unwrap(),
            user_data,
        );
    }
}

fn timer_finished(timer_id: u32) {
    let callback = {
        let ptr = timer_id as *mut Box<dyn FnOnce()>;
        unsafe { Box::from_raw(ptr) }
    };

    callback();
}

fn connection_open(id: u32) {
    let connection = unsafe { &mut *(usize::try_from(id).unwrap() as *mut Connection) };
    connection.open = true;
    if let Some(waker) = connection.waker.take() {
        waker.wake();
    }
}

fn connection_message(id: u32, ptr: u32, len: u32) {
    let connection = unsafe { &mut *(usize::try_from(id).unwrap() as *mut Connection) };

    let ptr = usize::try_from(ptr).unwrap();
    let len = usize::try_from(len).unwrap();

    let message: Box<[u8]> =
        unsafe { Box::from_raw(slice::from_raw_parts_mut(ptr as *mut u8, len)) };

    // Ignore empty message to avoid all sorts of problems.
    if message.is_empty() {
        return;
    }

    if connection.messages_queue.is_empty() {
        connection.messages_queue_first_offset = 0;
    }

    // TODO: add some limit to `messages_queue`, to avoid DoS attacks?

    connection.messages_queue.push_back(message);

    if let Some(waker) = connection.waker.take() {
        waker.wake();
    }
}

fn connection_closed(id: u32) {
    let connection = unsafe { &mut *(usize::try_from(id).unwrap() as *mut Connection) };
    connection.closed = true;
    if let Some(waker) = connection.waker.take() {
        waker.wake();
    }
}
