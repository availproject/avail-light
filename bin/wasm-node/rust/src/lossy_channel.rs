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

//! Utility module. Provides a channel equivalent to [`futures::channel::mpsc`] with a buffer
//! containing one element. If an item is sent on the channel while the channel is occupied,
//! the existing value is replaced with the new one.

// TODO: move somewhere else? in an external library maybe?

use futures::prelude::*;
use std::{
    pin::Pin,
    sync::{atomic, Arc, Mutex, Weak},
    task::{Context, Poll, Waker},
};

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        item: Mutex::new((None, None)),
        receiver_dead: atomic::AtomicBool::new(false),
    });

    let rx = Receiver {
        shared: Arc::downgrade(&shared),
    };
    let tx = Sender { shared };
    (tx, rx)
}

pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Sender<T> {
    pub fn send(&mut self, value: T) -> Result<(), ()> {
        if self.shared.receiver_dead.load(atomic::Ordering::Relaxed) {
            return Err(());
        }

        let waker = {
            let mut lock = self.shared.item.lock().unwrap();
            lock.0 = Some(value);
            lock.1.take()
        };

        if let Some(waker) = waker {
            waker.wake();
        }

        Ok(())
    }
}

pub struct Receiver<T> {
    shared: Weak<Shared<T>>,
}

impl<T> Stream for Receiver<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<T>> {
        let shared = match self.shared.upgrade() {
            Some(s) => s,
            None => return Poll::Ready(None),
        };

        let mut lock = shared.item.lock().unwrap();
        if let Some(item) = lock.0.take() {
            return Poll::Ready(Some(item));
        }

        match &mut lock.1 {
            Some(w) if w.will_wake(cx.waker()) => {}
            w @ _ => *w = Some(cx.waker().clone()),
        }

        Poll::Pending
    }
}

impl<T> stream::FusedStream for Receiver<T> {
    fn is_terminated(&self) -> bool {
        self.shared.upgrade().is_none()
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let shared = match self.shared.upgrade() {
            Some(s) => s,
            None => return,
        };

        shared.receiver_dead.store(true, atomic::Ordering::Relaxed)
    }
}

struct Shared<T> {
    item: Mutex<(Option<T>, Option<Waker>)>,
    receiver_dead: atomic::AtomicBool,
}
