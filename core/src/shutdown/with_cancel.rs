use std::{
	future::Future,
	pin::Pin,
	task::{Context, Poll},
};

use super::signal::Signal;

/// A wrapper for a future that is automatically canceled upon a shutdown trigger.
///
/// If the wrapped future completes before the shutdown is triggered,
/// the result of the original future is returned as `Ok(value)`.
///
/// However, if the shutdown occurs before the wrapped future completes,
/// the original future is terminated, and the shutdown reason is returned as `Err(reason)`
#[must_use = "futures stay idle unless you await them"]
pub struct WithCancel<T: Clone, F> {
	pub signal: Signal<T>,
	pub future: Result<F, T>,
}

impl<T: Clone, F: Future> Future for WithCancel<T, F> {
	type Output = Result<F::Output, T>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		// TODO: check `pin_utils` to avoid writing unsafe code when pinning to stack
		// if we are not moving the `future`,
		// requirements from `F` are never violated
		let WithCancel { future, signal } = unsafe { self.get_unchecked_mut() };

		// also here, we're never moving this future
		// we do drop it, but that is fine
		let wrapped_future = match future {
			Ok(future) => future,
			Err(reason) => return Poll::Ready(Err(reason.clone())),
		};

		// TODO: check `pin_utils` to avoid writing unsafe code when pinning to stack
		let pinned_future = unsafe { Pin::new_unchecked(wrapped_future) };
		// poll the wrapped future, check for it's progress
		if let Poll::Ready(value) = pinned_future.poll(cx) {
			return Poll::Ready(Ok(value));
		};

		// here wrapped future is still in the `Pending` state,
		// we need to whether a shutdown signal has been initiated in the meantime
		let Poll::Ready(reason) = Pin::new(signal).poll(cx) else {
			// future is still pending
			return Poll::Pending;
		};

		// shutdown signal happened, send back the reason
		// just to be safe, set the Result for the wrapped future reference
		*future = Err(reason.clone());
		Poll::Ready(Err(reason))
	}
}
