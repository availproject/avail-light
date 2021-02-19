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
    slice,
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
};

pub mod bindings;

/// Stops execution, throwing a string exception with the given content.
pub(crate) fn throw(message: String) -> ! {
    unsafe {
        bindings::throw(
            u32::try_from(message.as_bytes().as_ptr() as usize).unwrap(),
            u32::try_from(message.as_bytes().len()).unwrap(),
        );

        // Note: we could theoretically use `unreachable_unchecked` here, but this relies on the
        // fact that `ffi::throw` is correctly implemented, which isn't 100% guaranteed.
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

    impl futures::task::ArcWake for Waker {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            if arc_self
                .wake_up_registered
                .swap(true, atomic::Ordering::Relaxed)
            {
                return;
            }

            let arc_self = arc_self.clone();
            start_timer_wrap(Duration::new(0, 0), move || {
                if arc_self.done.load(atomic::Ordering::SeqCst) {
                    return;
                }

                let mut future = arc_self.future.try_lock().unwrap();
                arc_self
                    .wake_up_registered
                    .store(false, atomic::Ordering::SeqCst);
                match Future::poll(
                    future.as_mut(),
                    &mut Context::from_waker(&futures::task::waker_ref(&arc_self)),
                ) {
                    Poll::Ready(()) => {
                        arc_self.done.store(true, atomic::Ordering::SeqCst);
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

    futures::task::ArcWake::wake(waker);
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

/// Sets the content of the database to the given string.
pub(crate) fn database_save(content: &str) {
    unsafe {
        bindings::database_save(
            u32::try_from(content.as_bytes().as_ptr() as usize).unwrap(),
            u32::try_from(content.as_bytes().len()).unwrap(),
        );
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

/// WebSocket connected to a target.
pub struct WebSocket {
    /// If `Some`, [`bindings::websocket_close`] must be called. Set to a value after
    /// [`bindings::websocket_new`] returns success.
    id: Option<u32>,
    /// True if [`bindings::websocket_open`] has been called.
    open: bool,
    /// True if [`bindings::websocket_closed`] has been called.
    closed: bool,
    /// List of messages received through [`bindings::websocket_message`]. Must never contain
    /// empty messages.
    messages_queue: VecDeque<Box<[u8]>>,
    /// Position of the read cursor within the first element of [`WebSocket::messages_queue`].
    messages_queue_first_offset: usize,
    /// Waker to wake up whenever one of the fields above is modified.
    waker: Option<Waker>,
    /// Prevents the [`WebSocket`] from being unpinned.
    _pinned: marker::PhantomPinned,
}

impl WebSocket {
    /// Connects to the given URL. Returns a [`WebSocket`] on success.
    pub fn connect(url: &str) -> impl Future<Output = Result<Pin<Box<Self>>, ()>> {
        let mut pointer = Box::pin(WebSocket {
            id: None,
            open: false,
            closed: false,
            messages_queue: VecDeque::with_capacity(32),
            messages_queue_first_offset: 0,
            waker: None,
            _pinned: marker::PhantomPinned,
        });

        let id = u32::try_from(&*pointer as *const WebSocket as usize).unwrap();

        let ret_code = unsafe {
            bindings::websocket_new(
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

    /// Returns a buffer containing data received on the WebSocket.
    ///
    /// Never returns an empty buffer. If no data is available, this function waits until more
    /// data arrives.
    ///
    /// Returns `None` if the WebSocket has been closed.
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
    /// be returned by [`WebSocket::read_buffer`] the next time it is called.
    ///
    /// # Panic
    ///
    /// Panics if `bytes` is larger than the size of the buffer returned by
    /// [`WebSocket::read_buffer`].
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

    /// Queue of the given buffer as a WebSocket binary frame.
    pub fn send(self: &mut Pin<Box<Self>>, data: &[u8]) {
        unsafe {
            let this = Pin::get_unchecked_mut(self.as_mut());

            // WebSocket might have been closed, but API user hasn't detected it yet.
            if this.closed {
                return;
            }

            bindings::websocket_send(
                this.id.unwrap(),
                u32::try_from(data.as_ptr() as usize).unwrap(),
                u32::try_from(data.len()).unwrap(),
            );
        }
    }
}

impl fmt::Debug for WebSocket {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("WebSocket")
            .field(self.id.as_ref().unwrap())
            .finish()
    }
}

impl Drop for WebSocket {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            unsafe {
                bindings::websocket_close(id);
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

fn init(
    chain_specs_ptr: u32,
    chain_specs_len: u32,
    database_content_ptr: u32,
    database_content_len: u32,
    relay_chain_specs_ptr: u32,
    relay_chain_specs_len: u32,
    max_log_level: u32,
) {
    let chain_specs_ptr = usize::try_from(chain_specs_ptr).unwrap();
    let chain_specs_len = usize::try_from(chain_specs_len).unwrap();
    let database_content_ptr = usize::try_from(database_content_ptr).unwrap();
    let database_content_len = usize::try_from(database_content_len).unwrap();
    let relay_chain_specs_ptr = usize::try_from(relay_chain_specs_ptr).unwrap();
    let relay_chain_specs_len = usize::try_from(relay_chain_specs_len).unwrap();

    let chain_specs: Box<[u8]> = unsafe {
        Box::from_raw(slice::from_raw_parts_mut(
            chain_specs_ptr as *mut u8,
            chain_specs_len,
        ))
    };

    let chain_specs = String::from_utf8(Vec::from(chain_specs)).expect("non-utf8 chain specs");

    let database_content = if database_content_ptr != 0 {
        let data: Box<[u8]> = unsafe {
            Box::from_raw(slice::from_raw_parts_mut(
                database_content_ptr as *mut u8,
                database_content_len,
            ))
        };
        String::from_utf8(Vec::from(data)).ok()
    } else {
        None
    };

    let relay_chain_specs = if relay_chain_specs_ptr != 0 {
        let data: Box<[u8]> = unsafe {
            Box::from_raw(slice::from_raw_parts_mut(
                relay_chain_specs_ptr as *mut u8,
                relay_chain_specs_len,
            ))
        };
        Some(String::from_utf8(Vec::from(data)).expect("non-utf8 relay chain specs"))
    } else {
        None
    };

    let max_log_level = match max_log_level {
        0 => log::LevelFilter::Off,
        1 => log::LevelFilter::Error,
        2 => log::LevelFilter::Warn,
        3 => log::LevelFilter::Info,
        4 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };

    spawn_task(super::start_client(
        iter::once(super::ChainConfig {
            specification: chain_specs,
            database_content,
        })
        .chain(
            relay_chain_specs
                .into_iter()
                .map(|specification| super::ChainConfig {
                    specification,
                    database_content: None,
                }),
        ),
        max_log_level,
    ));
}

lazy_static::lazy_static! {
    static ref JSON_RPC_CHANNEL: (mpsc::UnboundedSender<Box<[u8]>>, futures::lock::Mutex<mpsc::UnboundedReceiver<Box<[u8]>>>) = {
        let (tx, rx) = mpsc::unbounded();
        (tx, futures::lock::Mutex::new(rx))
    };
}

fn json_rpc_send(ptr: u32, len: u32) {
    let ptr = usize::try_from(ptr).unwrap();
    let len = usize::try_from(len).unwrap();

    let request: Box<[u8]> =
        unsafe { Box::from_raw(slice::from_raw_parts_mut(ptr as *mut u8, len)) };
    JSON_RPC_CHANNEL.0.unbounded_send(request).unwrap();
}

/// Waits for the next JSON-RPC request coming from the JavaScript side.
// TODO: maybe tie the JSON-RPC system to a certain "client", instead of being global?
pub(crate) async fn next_json_rpc() -> Box<[u8]> {
    let mut lock = JSON_RPC_CHANNEL.1.lock().await;
    lock.next().await.unwrap()
}

/// Emit a JSON-RPC response or subscription notification in destination to the JavaScript side.
// TODO: maybe tie the JSON-RPC system to a certain "client", instead of being global?
pub(crate) fn emit_json_rpc_response(rpc: &str) {
    unsafe {
        bindings::json_rpc_respond(
            u32::try_from(rpc.as_ptr() as usize).unwrap(),
            u32::try_from(rpc.len()).unwrap(),
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

fn websocket_open(id: u32) {
    let websocket = unsafe { &mut *(usize::try_from(id).unwrap() as *mut WebSocket) };
    websocket.open = true;
    if let Some(waker) = websocket.waker.take() {
        waker.wake();
    }
}

fn websocket_message(id: u32, ptr: u32, len: u32) {
    let websocket = unsafe { &mut *(usize::try_from(id).unwrap() as *mut WebSocket) };

    let ptr = usize::try_from(ptr).unwrap();
    let len = usize::try_from(len).unwrap();

    let message: Box<[u8]> =
        unsafe { Box::from_raw(slice::from_raw_parts_mut(ptr as *mut u8, len)) };

    // Ignore empty message to avoid all sorts of problems.
    if message.is_empty() {
        return;
    }

    if websocket.messages_queue.is_empty() {
        websocket.messages_queue_first_offset = 0;
    }

    // TODO: add some limit to `messages_queue`, to avoid DoS attacks?

    websocket.messages_queue.push_back(message);

    if let Some(waker) = websocket.waker.take() {
        waker.wake();
    }
}

fn websocket_closed(id: u32) {
    let websocket = unsafe { &mut *(usize::try_from(id).unwrap() as *mut WebSocket) };
    websocket.closed = true;
    if let Some(waker) = websocket.waker.take() {
        waker.wake();
    }
}
