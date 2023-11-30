use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use super::{with_cancel::WithCancel, ControllerInner};

/// This future is created to wait for a shutdown signal.
///
/// It completes when the associated [`Controller`] instance triggers a shutdown.
///
/// The shutdown signal is thread-safe.
#[derive(Clone)]
pub struct Signal<T: Clone> {
	pub inner: Arc<Mutex<ControllerInner<T>>>,
}

impl<T: Clone> Signal<T> {
	/// Wraps a future, ensuring cancellation upon a shutdown trigger.
	///
	/// If the shutdown initiates before the wrapped future completes, the resulting future yields
	/// `Err(reason)` containing the shutdown reason. Upon successful completion of the wrapped future
	/// before a shutdown, it yields `Ok(val)`.

	pub fn with_cancel<F: Future>(&self, future: F) -> WithCancel<T, F> {
		WithCancel {
			signal: self.clone(),
			future: Ok(future),
		}
	}
}

impl<T: Clone> Future for Signal<T> {
	type Output = T;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.as_ref();
		let mut inner = this.inner.lock().unwrap();
		if let Some(reason) = inner.reason.clone() {
			Poll::Ready(reason)
		} else {
			inner.on_shutdown_trigger.push(cx.waker().clone());
			Poll::Pending
		}
	}
}
