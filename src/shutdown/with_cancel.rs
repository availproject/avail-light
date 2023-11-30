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
pub struct WithCancel<T: Clone, F> {
	pub signal: Signal<T>,
	pub future: Result<F, T>,
}

impl<T: Clone, F: Future> Future for WithCancel<T, F> {
	type Output = Result<F::Output, T>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		// TODO: check `pin_utils` to avoid writing unsafe code when pinning to stack
		// requirements from `F` are never violated, if we are
		// not moving the `future`
		let this = unsafe { self.get_unchecked_mut() };

		// also here, we're never moving those futures
		match &mut this.future {
			Err(err) => return Poll::Ready(Err(err.clone())),
			// we do drop it, but that is fine
			Ok(future) => {
				// TODO: check `pin_utils` to avoid writing unsafe code when pinning to stack
				let future = unsafe { Pin::new_unchecked(future) };
				if let Poll::Ready(val) = future.poll(cx) {
					return Poll::Ready(Ok(val));
				}
			},
		}

		// if future is still `Pending`, check if the shutdown signal has been given
		let shutdown = Pin::new(&mut this.signal).poll(cx);
		match shutdown {
			Poll::Ready(reason) => {
				this.future = Err(reason.clone());
				Poll::Ready(Err(reason))
			},
			Poll::Pending => Poll::Pending,
		}
	}
}
