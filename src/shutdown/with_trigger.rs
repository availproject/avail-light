use std::{
	future::Future,
	pin::Pin,
	task::{Context, Poll},
};

use super::TriggerToken;

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
			// requirements from `F` are never violated, if we are
			// not moving the `future`
			let this = self.get_unchecked_mut();
			match Pin::new_unchecked(&mut this.future).poll(cx) {
				Poll::Pending => Poll::Pending,
				Poll::Ready(val) => {
					this.trigger_token = None;
					Poll::Ready(val)
				},
			}
		}
	}
}
