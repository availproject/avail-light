use std::{
	future::Future,
	pin::Pin,
	task::{Context, Poll},
};

use super::TriggerToken;

/// A future wrapper that triggers a shutdown upon completion of the wrapped future,
/// or when the wrapped future is explicitly dropped.
#[must_use = "futures stay idle unless you await them"]
pub struct WithTrigger<T: Clone, F> {
	pub trigger_token: Option<TriggerToken<T>>,
	pub future: F,
}

impl<T: Clone, F: Future> Future for WithTrigger<T, F> {
	type Output = F::Output;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		// TODO: check `pin_utils` to avoid writing unsafe code when pinning to stack
		// Unsafe is used, since `std::future::Futures` are required to be `Unpin`
		unsafe {
			// if we are not moving the `future`,
			// requirements from `F` are never violated
			let WithTrigger {
				trigger_token,
				future,
			} = self.get_unchecked_mut();
			// poll the wrapped future
			let poll_result = Pin::new_unchecked(future).poll(cx);
			if poll_result.is_ready() {
				// once ready, drop the token to trigger the shutdown
				trigger_token.take();
			}
			// otherwise, return pending
			poll_result
		}
	}
}
