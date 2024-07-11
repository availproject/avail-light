use std::future::Future;
use std::pin::Pin;
use std::{
	sync::{Arc, Mutex},
	task::{Context, Poll},
};

use super::ControllerInner;

/// A future representing completion triggered by a shutdown condition.
///
/// The `Completed` struct wraps an inner state and implements the `Future` trait,
/// defining behavior for its completion based on a controlled shutdown mechanism.
///
/// This future completes when all tokens have been dropped and a reason for shutdown
/// has been provided. Otherwise, the future remains pending, registering the context's
/// waker for later notification upon shutdown completion.
pub struct Completed<T: Clone> {
	pub inner: Arc<Mutex<ControllerInner<T>>>,
}

impl<T: Clone> Future for Completed<T> {
	type Output = T;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let mutex_inner = &self.as_ref().inner;
		let mut inner = mutex_inner.lock().unwrap();
		// this future is done only when all tokens have been dropped
		// and when there is a reason for the shutdown
		if let (0, Some(reason)) = (inner.delay_tokens, inner.reason.as_ref()) {
			return Poll::Ready(reason.clone());
		}

		// always clone waker, so we don't end-up with staled ones
		inner.on_shutdown_complete.push(cx.waker().clone());
		Poll::Pending
	}
}
