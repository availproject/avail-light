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
		let this = unsafe { self.get_unchecked_mut() };

		// also here, we're never moving those futures
		match &mut this.future {
			Err(err) => Poll::Ready(Err(err.clone())),
			// we do drop it, but that is fine
			Ok(future) => {
				// TODO: check `pin_utils` to avoid writing unsafe code when pinning to stack
				let future = unsafe { Pin::new_unchecked(future) };
				// poll the wrapped future, check for it's progress
				match future.poll(cx) {
					Poll::Ready(val) => Poll::Ready(Ok(val)),
					Poll::Pending => {
						// if the wrapped future is still in the `Pending` state,
						// check whether a shutdown signal has been initiated in the meantime
						if let Poll::Ready(reason) = Pin::new(&mut this.signal).poll(cx) {
							this.future = Err(reason.clone());
							// shutdown signal happened, send back the reason
							return Poll::Ready(Err(reason));
						}
						// future is still pending
						Poll::Pending
					},
				}
			},
		}
	}
}
