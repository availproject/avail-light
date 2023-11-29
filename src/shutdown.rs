use std::{
	fmt::Debug,
	future::Future,
	mem,
	sync::{Arc, Mutex},
	task::Waker,
};

use self::{completed::Completed, with_delay::WithDelay};

mod completed;
mod with_delay;

#[derive(Clone)]
pub struct Controller<T: Clone> {
	inner: Arc<Mutex<ControllerInner<T>>>,
}

impl<T: Clone> Controller<T> {
	#[inline]
	pub fn new() -> Self {
		Self {
			inner: Arc::new(Mutex::new(ControllerInner::new())),
		}
	}

	pub fn is_shutdown_triggered(&self) -> bool {
		self.inner.lock().unwrap().reason.is_some()
	}

	pub fn is_shutdown_complete(&self) -> bool {
		let inner = self.inner.lock().unwrap();
		inner.reason.is_some() && inner.delay_tokens == 0
	}

	pub fn shutdown_reason(&self) -> Option<T> {
		self.inner.lock().unwrap().reason.clone()
	}

	pub fn trigger_shutdown(&self, reason: T) -> Result<(), ShutdownHasStarted<T>> {
		self.inner.lock().unwrap().shutdown(reason)
	}

	/// This function returns a future that resolves when the shutdown is fully completed.
	/// The resulting future can be cloned and safely dispatched to other threads or tasks.
	///
	/// The shutdown is considered complete when all counted instances of [`DelayTokens`]
	/// are dropped.
	pub fn completed_shutdown(&self) -> Completed<T> {
		Completed {
			inner: self.inner.clone(),
		}
	}

	/// Produces a token that holds back the shutdown as long as it exists.
	///
	/// The [`Controller`] instance keeps record of all created tokens.
	/// Tokens are thread-safe.
	/// All tokens must be dropped for shutdown to be able to complete.
	///
	/// For already completed shutdowns, this function returns error.
	fn delay_token(&self) -> Result<DelayToken<T>, ShutdownHasCompleted<T>> {
		let mut inner = self.inner.lock().unwrap();
		if inner.delay_tokens == 0 {
			if let Some(reason) = &inner.reason {
				return Err(ShutdownHasCompleted::new(reason.clone()));
			}
		}

		inner.increment_delay_tokens();
		Ok(DelayToken {
			inner: self.inner.clone(),
		})
	}
}

impl<T: Clone> Default for Controller<T> {
	fn default() -> Self {
		Self::new()
	}
}

pub struct ControllerInner<T> {
	reason: Option<T>,
	delay_tokens: usize,
	on_shutdown_trigger: Vec<Waker>,
	on_shutdown_complete: Vec<Waker>,
}

impl<T: Clone> ControllerInner<T> {
	fn new() -> Self {
		Self {
			reason: None,
			delay_tokens: 0,
			on_shutdown_trigger: Vec::new(),
			on_shutdown_complete: Vec::new(),
		}
	}

	fn increment_delay_tokens(&mut self) {
		self.delay_tokens += 1;
	}

	fn decrement_delay_tokens(&mut self) {
		self.delay_tokens = self.delay_tokens.saturating_sub(1);
		if self.delay_tokens == 0 {
			self.notify_shutdown_complete();
		}
	}

	fn notify_shutdown_complete(&mut self) {
		for waker in mem::take(&mut self.on_shutdown_complete) {
			waker.wake()
		}
	}

	fn shutdown(&mut self, reason: T) -> Result<(), ShutdownHasStarted<T>> {
		match &self.reason {
			Some(original) => Err(ShutdownHasStarted::new(original.clone(), reason)),
			None => {
				self.reason = Some(reason);
				for abort in std::mem::take(&mut self.on_shutdown_trigger) {
					abort.wake()
				}
				if self.delay_tokens == 0 {
					self.notify_shutdown_complete()
				}
				Ok(())
			},
		}
	}
}

/// The shutdown is delayed as long as this token exists.
///
/// This token is thread-safe, and can be sent to different threads and tasks.
///
/// * Important: For shutdown to complete, all clones must be dropped.

pub struct DelayToken<T: Clone> {
	inner: Arc<Mutex<ControllerInner<T>>>,
}

impl<T: Clone> DelayToken<T> {
	/// Wrapping a future defers shutdown completion until the wrapped future resolves or is dropped.
	///
	/// This function consumes the token to prevent retaining an unused token accidentally, which could
	/// indefinitely postpone shutdown. The resulting future transparently resolves with the value of the
	/// wrapped future.
	///
	/// However, the shutdown process remains pending until the future resolves or is dropped.
	pub fn wrap_future<F: Future>(self, future: F) -> WithDelay<T, F> {
		WithDelay {
			delay_token: Some(self),
			future,
		}
	}
}

impl<T: Clone> Clone for DelayToken<T> {
	fn clone(&self) -> Self {
		self.inner.lock().unwrap().increment_delay_tokens();
		DelayToken {
			inner: self.inner.clone(),
		}
	}
}

impl<T: Clone> Drop for DelayToken<T> {
	fn drop(&mut self) {
		self.inner.lock().unwrap().decrement_delay_tokens();
	}
}

#[derive(Debug, Clone)]
pub struct ShutdownHasStarted<T> {
	pub reason: T,
	pub ignored_reason: T,
}

impl<T> ShutdownHasStarted<T> {
	pub const fn new(reason: T, ignored_reason: T) -> Self {
		Self {
			reason,
			ignored_reason,
		}
	}
}

impl<T: std::fmt::Debug> std::error::Error for ShutdownHasStarted<T> {}

impl<T> std::fmt::Display for ShutdownHasStarted<T> {
	fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
		write!(
			f,
			"shutdown has already commenced, can not delay any further"
		)
	}
}

#[derive(Debug)]
pub struct ShutdownHasCompleted<T> {
	pub reason: T,
}

impl<T> ShutdownHasCompleted<T> {
	pub const fn new(reason: T) -> Self {
		Self { reason }
	}
}

impl<T: std::fmt::Debug> std::error::Error for ShutdownHasCompleted<T> {}

impl<T> std::fmt::Display for ShutdownHasCompleted<T> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "shutdown has been completed, can not delay any further")
	}
}

#[cfg(test)]
mod tests {
	use color_eyre::Result;
	use std::{future::Future, time::Duration};
	use tokio::{runtime, time::timeout};

	use crate::shutdown::Controller;

	// using custom runtime to create non-blocking promises instead of `[tokio::test]`,
	// ensuring predictable asynchronous operations without indefinite blocking
	#[track_caller]
	fn test_runtime(test: impl Future<Output = ()>) -> Result<()> {
		let runtime = runtime::Runtime::new()?;
		runtime.block_on(async move {
			let test_with_timeout = timeout(Duration::from_millis(100), test);
			assert!(test_with_timeout.await.is_ok());
		});

		Ok(())
	}

	#[test]
	fn shutdown_trigger() -> Result<()> {
		test_runtime(async {
			let controller = Controller::new();
			assert!(controller.trigger_shutdown(1).is_ok());
			assert!(controller.completed_shutdown().await == 1);
		})?;

		Ok(())
	}
}
