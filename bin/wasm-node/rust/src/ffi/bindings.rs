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

//! Imports and exports of the WebAssembly module.
//!
//! This module contains all the functions that tie together the Rust code and its host (i.e.
//! the JavaScript code, normally).
//!
//! The functions found in the `extern` block are the functions that the Rust code *imports*, and
//! need to be implemented on the host side and provided to the WebAssembly virtual machine. The
//! other functions are functions that the Rust code *exports*, and can be called by the host.
//!
//! # About wasi
//!
//! The Rust code is expected to be compiled for the `wasm32-wasi` target, and not just
//! `wasm32-unknown-unknown`. The `wasi` platform is used in order for example to obtain a source
//! of randomness.
//!
//! > **Note**: While wasi could theoretically also be used in order to obtain the current time,
//! >           the Wasi syscall cannot be implemented in pure JavaScript code at the moment, due
//! >           to `u64`s being unusable in Javascript. As such, alternatives are present in the
//! >           `extern` block below.
//!
//! Consequently, the exports found in the `extern` block below are not the only functions that
//! must be implemented. Several functions required by the Wasi ABI are also used. The best place
//! to find documentation at the moment is <https://docs.rs/wasi>.
//!

#[link(wasm_import_module = "smoldot")]
extern "C" {
    /// Must throw an exception. The message is a UTF-8 string found in the memory of the
    /// WebAssembly at offset `message_ptr` and with length `message_len`.
    ///
    /// After this function has been called, no further Wasm functions must be called again on
    /// this Wasm virtual machine. Explanation below.
    ///
    /// # About throwing and safety
    ///
    /// Rust programs can be configured in two panicking modes: `abort`, or `unwind`. Safe or
    /// unsafe Rust code must be written by keeping in mind that the execution of a function can
    /// be suddenly interrupted by a panic, but can rely on the fact that this panic will either
    /// completely abort the program, or unwind the stack. In the latter case, they can rely on
    /// the fact that `std::panic::catch_unwind` will catch this unwinding and let them perform
    /// some additional clean-ups.
    ///
    /// Calling `throw` is neither `abort`, because the JavaScript could call into the Wasm again
    /// later, nor `unwind`, because it isn't caught by `std::panic::catch_unwind`. By being
    /// neither of the two, it breaks the assumptions that some Rust codes might rely on for
    /// either correctness or safety.
    /// In order to solve this problem, we enforce that `throw` must behave like `abort`, and
    /// forbid calling into the Wasm virtual machine again.
    ///
    /// Beyond the `throw` function itself, any other FFI function that throws must similarly
    /// behave like `abort` and prevent any further execution.
    pub fn throw(message_ptr: u32, message_len: u32);

    /// Client is emitting a response to a previous JSON-RPC request sent using [`json_rpc_send`].
    /// Also used to send subscriptions notifications.
    ///
    /// The response or notification is a UTF-8 string found in the memory of the WebAssembly
    /// virtual machine at offset `ptr` and with length `len`.
    pub fn json_rpc_respond(ptr: u32, len: u32);

    /// Client is emitting a log entry.
    ///
    /// Each log entry is made of a log level (1 = Error, 2 = Warn, 3 = Info, 4 = Debug,
    /// 5 = Trace), a log target (e.g. "network"), and a log message.
    ///
    /// The log target and message is a UTF-8 string found in the memory of the WebAssembly
    /// virtual machine at offset `ptr` and with length `len`.
    pub fn log(level: u32, target_ptr: u32, target_len: u32, message_ptr: u32, message_len: u32);

    /// Must return the number of milliseconds that have passed since the UNIX epoch, ignoring
    /// leap seconds.
    ///
    /// This is typically implemented by calling `Date.now()`.
    ///
    /// See https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Date/now
    ///
    /// > **Note**: Ideally this function isn't needed. The wasi target supports clocks through
    /// >           the `clock_time_get` syscall. However, since `clock_time_get` uses u64s, and
    /// >           browsers don't support u64s, using it causes an unbypassable exception. See
    /// >           also <https://github.com/dcodeIO/webassembly/issues/26#issuecomment-410157370>.
    pub fn unix_time_ms() -> f64;

    /// Must return the number of milliseconds that have passed since an arbitrary point in time.
    ///
    /// Contrary to [`unix_time_ms`], the returned value must never be inferior to a value
    /// previously returned. Consequently, this must not be implemented using `Date.now()`, whose
    /// value can decrease if the user adjusts their machine's clock, but rather with
    /// `Performance.now()` or similar.
    ///
    /// See https://developer.mozilla.org/fr/docs/Web/API/Performance/now
    ///
    /// > **Note**: Ideally this function isn't needed. The wasi target supports clocks through
    /// >           the `clock_time_get` syscall. However, since `clock_time_get` uses u64s, and
    /// >           browsers don't support u64s, using it causes an unbypassable exception. See
    /// >           also <https://github.com/dcodeIO/webassembly/issues/26#issuecomment-410157370>.
    pub fn monotonic_clock_ms() -> f64;

    /// After at least `milliseconds` milliseconds have passed, must call [`timer_finished`] with
    /// the `id` passed as parameter.
    ///
    /// When [`timer_finished`] is called, the value of [`monotonic_clock_ms`] must have increased
    /// by at least the given number of `milliseconds`.
    ///
    /// If `milliseconds` is 0, [`timer_finished`] should be called as soon as possible.
    pub fn start_timer(id: u32, milliseconds: f64);

    /// Client wants to set the content of the database to a UTF-8 string found at offset `ptr`
    /// and with length `len`.
    ///
    /// The entire content of the database should be replaced with that string.
    ///
    /// This value is meant to later be passed to [`init`] when restarting the client.
    ///
    /// Saving the database is entirely optional, and it is legal to simply do nothing.
    pub fn database_save(ptr: u32, len: u32);

    /// Must initialize a new WebSocket connection that tries to connect to the given URL.
    ///
    /// The URL is a UTF-8 string found in the WebAssembly memory at offset `url_ptr` and with
    /// `url_len` bytes. The string is in a format suitable for
    /// [`new WebSocket()`](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket/WebSocket).
    ///
    /// The `id` parameter is an identifier for this WebSocket, as chosen by the Rust code. It
    /// must be passed on every interaction with this WebSocket.
    ///
    /// Returns 0 to indicate success, or 1 to indicate that an error happened. If an error is
    /// returned, the `id` doesn't correspond to anything.
    ///
    /// > **Note**: If you implement this function using `new WebSocket()`, please keep in mind
    /// >           that exceptions should be caught and turned into an error code.
    ///
    /// At any time, a WebSocket can be in one of the three following states:
    ///
    /// - `Opening` (initial state)
    /// - `Open`
    /// - `Closed`
    ///
    /// When in the `Opening` or `Open` state, the WebSocket can transition to the `Closed` state
    /// if the remote closes the connection or refuses the connection altogether. When that
    /// happens, [`websocket_closed`] must be called. Once in the `Closed` state, the WebSocket
    /// cannot transition back to another state.
    ///
    /// Initially in the `Opening` state, the WebSocket can transition to the `Open` state if the
    /// remote accepts the connection. When that happens, [`websocket_open`] must be called.
    ///
    /// When in the `Open` state, the WebSocket can receive messages. When a message is received,
    /// [`alloc`] must be called in order to allocate memory for this message, then
    /// [`websocket_message`] must be called with the pointer returned by [`alloc`].
    pub fn websocket_new(id: u32, url_ptr: u32, url_len: u32) -> u32;

    /// Close a WebSocket previously initialized with [`websocket_new`].
    ///
    /// This destroys the identifier passed as parameter. This identifier must never be passed
    /// through the FFI boundary, unless the same identifier is later allocated again with
    /// [`websocket_new`].
    ///
    /// The WebSocket connection must be closed in the background. The Rust code isn't interested
    /// in incoming messages from this WebSocket anymore.
    ///
    /// > **Note**: If implemented using the `WebSocket` API, remember to unregister event
    /// >           handlers before calling `WebSocket.close()`.
    pub fn websocket_close(id: u32);

    /// Queues data on the given WebSocket. The data is found in the memory of the WebAssembly
    /// virtual machine, at the given pointer. The data must be sent as a binary frame.
    ///
    /// The WebSocket must currently be in the `Open` state. See the documentation of
    /// [`websocket_new`] for details.
    pub fn websocket_send(id: u32, ptr: u32, len: u32);
}

/// Allocates a buffer of the given length, with an alignment of 1.
///
/// This must be used in the context of [`init`].
#[no_mangle]
pub extern "C" fn alloc(len: u32) -> u32 {
    super::alloc(len)
}

/// Initializes the client.
///
/// Use [`alloc`] to allocate either one to three buffers: one for the chain specs, an optional
/// one for the database content, and an optional one for the chain specs of the relay chain if
/// the chain is a parachain.
/// The buffers **must** have been allocated with [`alloc`]. They are freed when this function is
/// called.
///
/// Write the chain specs, the database content, and the relay chain specs in these three buffers.
/// Then, pass the pointer and length of these buffers to this function.
/// Pass `0` for `database_content_ptr` and `database_content_len` if the database is empty.
/// Pass `0` for `relay_chain_specs_ptr` and `relay_chain_specs_len` if the chain is not a
/// parachain.
///
/// The client will emit log messages by calling the [`log()`] function, provided the log level is
/// inferior or equal to the value of `max_log_level` passed here.
#[no_mangle]
pub extern "C" fn init(
    chain_specs_ptr: u32,
    chain_specs_len: u32,
    database_content_ptr: u32,
    database_content_len: u32,
    relay_chain_specs_ptr: u32,
    relay_chain_specs_len: u32,
    max_log_level: u32,
) {
    super::init(
        chain_specs_ptr,
        chain_specs_len,
        database_content_ptr,
        database_content_len,
        relay_chain_specs_ptr,
        relay_chain_specs_len,
        max_log_level,
    )
}

/// Emit a JSON-RPC request. If the initialization (see [`init`]) hasn't been started or hasn't
/// finished yet, the request will still be queued.
///
/// A buffer containing a UTF-8 JSON-RPC request must be passed as parameter. The format of the
/// JSON-RPC requests is described in
/// [the standard JSON-RPC 2.0 specifications](https://www.jsonrpc.org/specification). A pub-sub
/// extension is supported.
///
/// The buffer passed as parameter **must** have been allocated with [`alloc`]. It is freed when
/// this function is called.
///
/// Responses and subscriptions notifications are sent back using [`json_rpc_respond`].
#[no_mangle]
pub extern "C" fn json_rpc_send(text_ptr: u32, text_len: u32) {
    super::json_rpc_send(text_ptr, text_len)
}

/// Must be called in response to [`start_timer`] after the given duration has passed.
#[no_mangle]
pub extern "C" fn timer_finished(timer_id: u32) {
    super::timer_finished(timer_id);
}

/// Called by the JavaScript code if the WebSocket switches to the `Open` state. The WebSocket
/// must be in the `Opening` state.
///
/// Must only be called once per WebSocket object.
///
/// See also [`websocket_open`].
#[no_mangle]
pub extern "C" fn websocket_open(id: u32) {
    super::websocket_open(id);
}

/// Notify of a message being received on the WebSocket. The WebSocket must be in the `Open` state.
///
/// See also [`websocket_open`].
///
/// The buffer **must** have been allocated with [`alloc`]. It is freed when this function is
/// called.
#[no_mangle]
pub extern "C" fn websocket_message(id: u32, ptr: u32, len: u32) {
    super::websocket_message(id, ptr, len)
}

/// Can be called at any point by the JavaScript code if the WebSocket switches to the `Closed`
/// state.
///
/// Must only be called once per WebSocket object.
///
/// See also [`websocket_open`].
#[no_mangle]
pub extern "C" fn websocket_closed(id: u32) {
    super::websocket_closed(id)
}
