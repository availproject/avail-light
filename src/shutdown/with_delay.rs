use std::{
	future::Future,
	pin::Pin,
	task::{Context, Poll},
};

use super::DelayToken;

/// This future defers shutdown completion until its execution finishes,
/// or until itself is dropped.
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
			// requirements from `F` are never violated, if we are
			// not moving the `future`
			let this = self.get_unchecked_mut();
			match Pin::new_unchecked(&mut this.future).poll(cx) {
				Poll::Pending => Poll::Pending,
				Poll::Ready(val) => {
					this.delay_token = None;
					Poll::Ready(val)
				},
			}
		}
	}
}
