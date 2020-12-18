// Substrate-lite
// Copyright (C) 2019-2020  Parity Technologies (UK) Ltd.
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

//! Augments an implementation of `AsyncRead` and `AsyncWrite` with a read buffer and a write
//! buffer.
//!
//! While this module is generic, the targeted use-case is TCP connections.

// TODO: usage and example

use core::{fmt, pin::Pin, task::Poll};
use futures::{
    io::{AsyncRead, AsyncWrite},
    prelude::*,
};
use std::io;

/// Holds an implementation of `AsyncRead` and `AsyncWrite`, alongside with a read buffer and a
/// write buffer.
#[pin_project::pin_project]
pub struct WithBuffers<T> {
    /// Actual socket to read from/write to.
    #[pin]
    socket: T,
    /// Error that has happened on the socket, if any.
    error: Option<io::Error>,
    /// Storage for data read from the socket.
    read_buffer: Box<[u8]>,
    /// Offset in `read_buffer` of the buffer returned by [`WithBuffers::buffers`].
    /// If this is equal to `socket_in_cursor_start`, then `buffers` should return an empty slice.
    read_buffer_processed_cursor: usize,
    /// Offset in `read_buffer` where the socket will put incoming data. Also equal to the end
    /// of the buffer returned by [`WithBuffers::buffers`].
    socket_in_cursor_start: usize,
    /// True if reading from the socket has returned `Ok(0)` earlier, in other words "end of
    /// file".
    read_closed: bool,
    /// Storage for data to write to the socket.
    write_buffer: Box<[u8]>,
    /// Offset in `write_buffer` of the data ready to be sent out on the socket.
    write_ready_start: usize,
    /// Offset in `write_buffer` of the end of the data ready to be sent out on the socket.
    /// Can be inferior to `write_ready_start` in case the data wraps around.
    /// If equal to `write_ready_start`, that means no data is ready.
    write_ready_end: usize,
    /// True if the user has called [`WithBuffers::close`] earlier.
    write_closed: bool,
    /// True if the user has called [`WithBuffers::close`] earlier, and the socket still has to
    /// be closed.
    close_pending: bool,
    /// True if data has been written on the socket and the socket needs to be flushed.
    flush_pending: bool,
}

impl<T> WithBuffers<T> {
    /// Initializes a new [`WithBuffers`] with the given socket.
    ///
    /// The socket must still be open in both directions.
    pub fn new(socket: T) -> Self {
        WithBuffers {
            socket,
            error: None,
            read_buffer: vec![0; 8192].into_boxed_slice(),
            read_buffer_processed_cursor: 0,
            socket_in_cursor_start: 0,
            read_closed: false,
            write_buffer: vec![0; 8192].into_boxed_slice(),
            write_ready_start: 0,
            write_ready_end: 0,
            write_closed: false,
            close_pending: false,
            flush_pending: false,
        }
    }

    /// Returns a buffer containing data read from the socket, and a buffer containing data
    /// destined to be written to the socket.
    ///
    /// If an error happend on the socket earlier, it is returned instead.
    ///
    /// The read buffer is an `Option` containing `None` if the reading side of the socket has
    /// been closed. The write buffer is an `Option` containing `None` if the writing side of the
    /// socket has been closed.
    ///
    /// If this method returns `Ok(None, None)`, the socket is now useless and can be dropped.
    ///
    /// > **Note**: The write buffer is `None` only after the writing side has actually been
    /// >           closed, not immediately after calling [`WithBuffers::close`].
    ///
    /// This method is idempotent. You should later call [`WithBuffers::advance`] to advance the
    /// cursor in these buffers.
    pub fn buffers(
        &mut self,
    ) -> Result<(Option<(&[u8], &[u8])>, Option<(&mut [u8], &mut [u8])>), &io::Error> {
        if let Some(error) = self.error.as_ref() {
            return Err(error);
        }

        let read_buffer = if self.read_closed
            && self.read_buffer_processed_cursor == self.socket_in_cursor_start
        {
            None
        } else if self.read_buffer_processed_cursor <= self.socket_in_cursor_start {
            Some((
                &self.read_buffer[self.read_buffer_processed_cursor..self.socket_in_cursor_start],
                &[][..],
            ))
        } else {
            let (buf2, buf1) = self.read_buffer.split_at(self.read_buffer_processed_cursor);
            let buf2 = &buf2[..self.socket_in_cursor_start];
            Some((buf1, buf2))
        };

        let write_buffer = if self.write_closed {
            if self.close_pending {
                Some((&mut [][..], &mut [][..]))
            } else {
                None
            }
        } else if self.write_ready_end < self.write_ready_start {
            Some((
                &mut self.write_buffer[self.write_ready_end..self.write_ready_start - 1],
                &mut [][..],
            ))
        } else if self.write_ready_start == 0 {
            let end = self.write_buffer.len() - 1;
            Some((
                &mut self.write_buffer[self.write_ready_end..end],
                &mut [][..],
            ))
        } else {
            let (buf2, buf1) = self.write_buffer.split_at_mut(self.write_ready_end);
            let buf2 = &mut buf2[..self.write_ready_start - 1];
            Some((buf1, buf2))
        };

        Ok((read_buffer, write_buffer))
    }

    /// Advances the cursors of the buffers.
    ///
    /// Discards the first `read_n` bytes of the read buffer, and the first `write_n` bytes of the
    /// write buffer. The bytes discarded from the write buffer will later be sent when
    /// [`WithBuffers::process`] is called.
    ///
    /// # Panic
    ///
    /// Panics if `read_n` or `write_n` are larger than the lengths of the buffers returned by
    /// [`WithBuffers::buffers`].
    /// Panics if [`WithBuffers::buffers`] has returned an error.
    ///
    pub fn advance(&mut self, read_n: usize, write_n: usize) {
        // Read cursor.
        if self.read_buffer_processed_cursor > self.socket_in_cursor_start {
            self.read_buffer_processed_cursor = self
                .read_buffer_processed_cursor
                .checked_add(read_n)
                .unwrap();
            if self.read_buffer_processed_cursor >= self.read_buffer.len() {
                self.read_buffer_processed_cursor -= self.read_buffer.len();
                assert!(self.read_buffer_processed_cursor <= self.socket_in_cursor_start);
            }
        } else {
            self.read_buffer_processed_cursor = self
                .read_buffer_processed_cursor
                .checked_add(read_n)
                .unwrap();
            assert!(self.read_buffer_processed_cursor <= self.socket_in_cursor_start);
        }

        // Write cursor.
        if self.write_closed {
            assert_eq!(write_n, 0);
        } else if self.write_ready_end < self.write_ready_start {
            self.write_ready_end = self.write_ready_end.checked_add(write_n).unwrap();
            assert!(self.write_ready_end < self.write_ready_start);
        } else {
            self.write_ready_end = self.write_ready_end.checked_add(write_n).unwrap();
            if self.write_ready_end >= self.write_buffer.len() {
                self.write_ready_end -= self.write_buffer.len();
                assert!(self.write_ready_end < self.write_ready_start);
            } else {
                assert!(self.write_ready_end >= self.write_ready_start);
            }
        }
    }

    /// Indicate that the writing side of the connection must be closed. The write buffer returned
    /// by [`WithBuffers::buffers`] will then return `Some(&mut [])`. After the writing side has
    /// actually been closed, it will return `None`.
    pub fn close(&mut self) {
        // TODO: should we not panic here or something? or return an error?
        if !self.write_closed {
            self.write_closed = true;
            self.close_pending = true;
        }
    }

    /// True if [`WithBuffers::close`] has been called earlier.
    pub fn is_closed(&self) -> bool {
        self.write_closed
    }
}

impl<T> WithBuffers<T>
where
    T: AsyncWrite,
{
    /// Wait until the socket has been properly closed.
    ///
    /// Implicitly calls [`WithBuffers::close`] if it hasn't been done.
    ///
    /// Has no effect if an error has happened in the past on the socket.
    ///
    /// This is similar to [`WithBuffers::process`], except that no reading or writing is ever
    /// performed.
    pub async fn flush_close(self: Pin<&mut Self>) {
        let mut this = self.project();

        if !*this.write_closed {
            *this.write_closed = true;
            debug_assert!(!*this.close_pending);
            *this.close_pending = true;
        }

        future::poll_fn(move |cx| {
            if !*this.close_pending || this.error.is_some() {
                return Poll::Ready(());
            }

            match AsyncWrite::poll_close(this.socket.as_mut(), cx) {
                Poll::Ready(Ok(())) => {
                    *this.close_pending = false;
                    return Poll::Ready(());
                }
                Poll::Ready(Err(err)) => {
                    *this.error = Some(err);
                    return Poll::Ready(());
                }
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }
}

impl<T> WithBuffers<T>
where
    T: AsyncRead + AsyncWrite,
{
    /// Waits either for data to be received or for data to be written out.
    ///
    /// Also returns if an error happened on the socket. If an error has already happened in the
    /// past, returns immediately.
    ///
    /// After this function has returned, the buffers returned by [`WithBuffers::buffers`] might
    /// have changed. The read buffer might have been extended with more data, and the write buffer
    /// might have been extended.
    pub async fn process(self: Pin<&mut Self>) {
        let mut this = self.project();

        future::poll_fn(move |cx| {
            if this.error.is_some() {
                return Poll::Ready(());
            }

            // If still `true` at the end of the function, `Poll::Pending` is returned.
            let mut pending = true;

            // All the reading is skipped if the reading side of the socket is closed or if
            // `socket_in_cursor_start == read_buffer_processed_cursor - 1`, as we don't want the
            // socket reading to catch up with the processing.
            if !*this.read_closed
                && *this.socket_in_cursor_start
                    != this
                        .read_buffer_processed_cursor
                        .checked_sub(1)
                        .unwrap_or(this.read_buffer.len() - 1)
            {
                let read_result = if *this.socket_in_cursor_start
                    < *this.read_buffer_processed_cursor
                {
                    debug_assert_ne!(
                        *this.socket_in_cursor_start,
                        *this.read_buffer_processed_cursor - 1
                    );
                    AsyncRead::poll_read(
                        this.socket.as_mut(),
                        cx,
                        &mut this.read_buffer
                            [*this.socket_in_cursor_start..*this.read_buffer_processed_cursor - 1],
                    )
                } else if *this.read_buffer_processed_cursor == 0 {
                    debug_assert!(*this.socket_in_cursor_start < this.read_buffer.len() - 1);
                    let end = this.read_buffer.len() - 1;
                    AsyncRead::poll_read(
                        this.socket.as_mut(),
                        cx,
                        &mut this.read_buffer[*this.socket_in_cursor_start..end],
                    )
                } else {
                    let (buf2, buf1) = this.read_buffer.split_at_mut(*this.socket_in_cursor_start);
                    let buf1 = io::IoSliceMut::new(buf1);
                    let buf2 =
                        io::IoSliceMut::new(&mut buf2[..*this.read_buffer_processed_cursor - 1]);
                    AsyncRead::poll_read_vectored(this.socket.as_mut(), cx, &mut [buf1, buf2])
                };

                match read_result {
                    Poll::Pending => {}
                    Poll::Ready(Ok(0)) => {
                        *this.read_closed = true;
                        pending = false;
                    }
                    Poll::Ready(Ok(n)) => {
                        // Note that fewer checks are being performed compared to
                        // [`WithBuffers::advance`], as it is assumed that the implementation of
                        // `AsyncRead` is working.
                        *this.socket_in_cursor_start += n;
                        if *this.socket_in_cursor_start >= this.read_buffer.len() {
                            *this.socket_in_cursor_start -= this.read_buffer.len();
                        }
                        assert_ne!(
                            *this.socket_in_cursor_start,
                            *this.read_buffer_processed_cursor
                        );
                        pending = false;
                    }
                    Poll::Ready(Err(err)) => {
                        *this.error = Some(err);
                        return Poll::Ready(());
                    }
                };
            }

            loop {
                if *this.write_ready_start != *this.write_ready_end {
                    let write_result = if *this.write_ready_start < *this.write_ready_end {
                        AsyncWrite::poll_write(
                            this.socket.as_mut(),
                            cx,
                            &mut this.write_buffer[*this.write_ready_start..*this.write_ready_end],
                        )
                    } else {
                        let (buf2, buf1) = this.write_buffer.split_at(*this.write_ready_start);
                        let buf1 = io::IoSlice::new(buf1);
                        let buf2 = io::IoSlice::new(&buf2[..*this.write_ready_end]);
                        AsyncWrite::poll_write_vectored(this.socket.as_mut(), cx, &[buf1, buf2])
                    };

                    match write_result {
                        Poll::Ready(Ok(0)) => {
                            panic!(); // TODO: what does that mean?
                        }
                        Poll::Ready(Ok(n)) => {
                            pending = false;
                            *this.flush_pending = true;
                            *this.write_ready_start += n;
                            if *this.write_ready_start >= this.write_buffer.len() {
                                *this.write_ready_start -= this.write_buffer.len();
                            }
                        }
                        Poll::Ready(Err(err)) => {
                            *this.error = Some(err);
                            return Poll::Ready(());
                        }
                        Poll::Pending => break,
                    };
                } else if *this.flush_pending {
                    match AsyncWrite::poll_flush(this.socket.as_mut(), cx) {
                        Poll::Ready(Ok(())) => {
                            *this.flush_pending = false;
                            pending = false;
                        }
                        Poll::Ready(Err(err)) => {
                            *this.error = Some(err);
                            return Poll::Ready(());
                        }
                        Poll::Pending => break,
                    }
                } else if *this.close_pending {
                    match AsyncWrite::poll_close(this.socket.as_mut(), cx) {
                        Poll::Ready(Ok(())) => {
                            *this.close_pending = false;
                            pending = false;
                            break;
                        }
                        Poll::Ready(Err(err)) => {
                            *this.error = Some(err);
                            return Poll::Ready(());
                        }
                        Poll::Pending => break,
                    }
                } else {
                    break;
                }
            }

            if !pending {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

impl<T: fmt::Debug> fmt::Debug for WithBuffers<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("WithBuffers").field(&self.socket).finish()
    }
}

// TODO: tests
