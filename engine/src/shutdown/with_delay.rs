use std::{
	future::Future,
	pin::Pin,
	task::{Context, Poll},
};

use super::DelayToken;

/// This wrapper for a future delays the completion of shutdown until the execution of the wrapped future
/// completes or until the wrapped future is dropped.
#[must_use = "futures stay idle unless you await them"]
pub struct WithDelay<T: Clone, F> {
	pub delay_token: Option<DelayToken<T>>,
	pub future: F,
}

impl<T: Clone, F: Future> Future for WithDelay<T, F> {
	type Output = F::Output;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		// since there's no point in pinning this promise to the Heap,
		// we'll Pin it to the Stack
		// Stack pinning will always rely on guarantees we give when writing `unsafe`
		// this will always be `unsafe` if our type implements `!Unpin`
		// TODO: check `pin_utils` to avoid writing unsafe code when pinning to stack

		// Unsafe is used, since `std::future::Futures` are required to be `Unpin`
		unsafe {
			// if we are not moving the `future`,
			// requirements from `F` are never violated
			let WithDelay {
				delay_token,
				future,
			} = self.get_unchecked_mut();
			// poll the wrapped future
			let poll_result = Pin::new_unchecked(future).poll(cx);
			if poll_result.is_ready() {
				// once ready, drop the token so the shutdown is not delayed any more
				delay_token.take();
			}
			// otherwise, return pending
			poll_result
		}
	}
}
