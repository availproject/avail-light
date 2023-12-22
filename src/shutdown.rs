use std::{
	fmt::Debug,
	future::Future,
	mem,
	sync::{Arc, Mutex},
	task::Waker,
};

use self::{
	completed::Completed, signal::Signal, with_cancel::WithCancel, with_delay::WithDelay,
	with_trigger::WithTrigger,
};

mod completed;
mod signal;
mod with_cancel;
mod with_delay;
mod with_trigger;

#[derive(Clone)]
/// Shutdown controller for graceful shutdowns in async code.
///
/// This controller addresses problems such as:
/// * Stopping running futures when a shutdown signal is triggered.
/// * Waiting for futures to complete potential clean-ups.
/// * Retrieving the shutdown reason after shutdown triggers.
///
/// The Controller can be cloned and is thread-safe.
///
/// Caution: `JoinHandles` behave differently than regular futures.
/// Dropping a `JoinHandle` might detach it from the task, leaving the task running.
/// Take care to wrap futures before spawning to avoid potential data loss on shutdown.
pub struct Controller<T: Clone> {
	inner: Arc<Mutex<ControllerInner<T>>>,
}

impl<T: Clone> Controller<T> {
	#[inline]
	/// Instantiate new shutdown controller.
	pub fn new() -> Self {
		Self {
			inner: Arc::new(Mutex::new(ControllerInner::new())),
		}
	}

	/// Checks if the shutdown has been triggered.
	pub fn is_shutdown_triggered(&self) -> bool {
		self.inner.lock().unwrap().reason.is_some()
	}

	/// Checks if the shutdown has completed.
	pub fn is_shutdown_completed(&self) -> bool {
		let inner = self.inner.lock().unwrap();
		inner.reason.is_some() && inner.delay_tokens == 0
	}

	/// Gets the shutdown reason, for the triggered shutdown.
	///
	/// Returns [`None`] if the shutdown has not been triggered yet.
	pub fn shutdown_reason(&self) -> Option<T> {
		self.inner.lock().unwrap().reason.clone()
	}

	/// Triggers the shutdown to begin.
	///
	/// This will make all [`Signal`] and [`WithCancel`] futures to be resolved.
	///
	/// The shutdown will not complete until every [`DelayToken`] has been dropped.
	///
	/// If the shutdown has already been started, this function returns an error.
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

	/// Awaits the triggering of the shutdown signal.
	///
	/// Returns a future that completes when the shutdown is initiated.
	/// The future is designed to be thread-safe.
	///
	/// If the shutdown signal is already triggered, the returned future immediately resolves.
	///
	/// To automatically cancel a future upon receiving the shutdown signal, use `Signal::with_cancel()`.
	/// This method is equivalent to `Self::with_cancel()`.
	pub fn triggered_shutdown(&self) -> Signal<T> {
		Signal {
			inner: self.inner.clone(),
		}
	}

	/// Wraps a future to cancel it upon a triggered shutdown.
	///
	/// The returned future completes with `Err(reason)` if the shutdown is triggered before the wrapped future.
	/// If the wrapped future completes before a shutdown, it yields `Ok(value)`.
	pub fn with_cancel<F: Future>(&self, future: F) -> WithCancel<T, F> {
		self.triggered_shutdown().with_cancel(future)
	}

	/// Wraps a future to defer shutdown completion until the wrapped future completes or is dropped.
	///
	/// Returns a future that transparently completes with the value of the wrapped future.
	/// The shutdown process remains pending until the wrapped future completes or is dropped.
	///
	/// If the shutdown has already been finalized, an error will be returned.
	pub fn with_delay<F: Future>(
		&self,
		future: F,
	) -> Result<WithDelay<T, F>, ShutdownHasCompleted<T>> {
		Ok(self.delay_token()?.with_future(future))
	}

	/// Wraps a future to trigger a shutdown when the future completes or when it is dropped.
	pub fn with_trigger<F: Future>(&self, reason: T, future: F) -> WithTrigger<T, F> {
		self.trigger_token(reason).with_future(future)
	}

	/// Produces a token that delays the shutdown as long as it exists.
	///
	/// The [`Controller`] instance manages all created tokens, ensuring thread-safe operations.
	/// For the shutdown to complete, all tokens must be dropped.
	///
	/// If the shutdown has already completed, this function returns an error.
	///
	/// Consider using [`Self::with_future()`] to delay the shutdown until a future completes.
	pub fn delay_token(&self) -> Result<DelayToken<T>, ShutdownHasCompleted<T>> {
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

	/// Produces a token that triggers a shutdown upon being dropped.
	///
	/// Dropping a [`TriggerToken`] automatically initiates a shutdown process.
	/// This behaviour applies universally to all tokens of this type.
	/// For instance, cloning this token five times and dropping one of them will trigger a shutdown.
	///
	/// Additionally, [`Self::with_future()`] allows for wrapping a future, which will ensure that a
	/// shutdown is triggered either upon the future's completion of if it dropped prematurely.
	pub fn trigger_token(&self, reason: T) -> TriggerToken<T> {
		TriggerToken {
			reason: Arc::new(Mutex::new(Some(reason))),
			inner: self.inner.clone(),
		}
	}
}

impl<T: Clone> Default for Controller<T> {
	fn default() -> Self {
		Self::new()
	}
}

pub struct ControllerInner<T> {
	/// The reason why shutdown is happening.
	reason: Option<T>,

	/// Count of all delay tokens in existence.
	///
	/// Must reach 0 before shutdown can complete.
	delay_tokens: usize,

	/// Tasks that need to be awaken when shutdown is triggered.
	on_shutdown_trigger: Vec<Waker>,

	/// Tasks that need to be awaken when the shutdown is complete.
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
	pub fn with_future<F: Future>(self, future: F) -> WithDelay<T, F> {
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

#[derive(Clone)]
/// This token initiates a graceful shutdown upon being dropped.
///
/// It guarantees thread safety.
///
/// Dropping any cloned tokens triggers the shutdown process,
/// regardless of the existence of other clones.
pub struct TriggerToken<T: Clone> {
	reason: Arc<Mutex<Option<T>>>,
	inner: Arc<Mutex<ControllerInner<T>>>,
}

impl<T: Clone> TriggerToken<T> {
	/// Safely wraps a future, ensuring a controlled shutdown upon completion
	/// or when dropped.
	///
	/// By consuming the [`TriggerToken`] within this wrapper, it prevents accidental
	/// token dropping that could prematurely trigger a shutdown while a future is being
	/// processed.
	///
	/// To retain the token for further use, consider making a clone before using wrap function.
	pub fn with_future<F: Future>(self, future: F) -> WithTrigger<T, F> {
		WithTrigger {
			trigger_token: Some(self),
			future,
		}
	}

	/// Discard the token without invoking its drop behavior.
	pub fn forget(self) {
		std::mem::forget(self)
	}
}

impl<T: Clone> Drop for TriggerToken<T> {
	fn drop(&mut self) {
		let mut inner = self.inner.lock().unwrap();
		if let Some(reason) = self.reason.lock().unwrap().take() {
			_ = inner.shutdown(reason);
		}
	}
}

/// Error returned when [`Controller`] instance tries to trigger the shutdown
/// multiple times on the same controller instance.
#[derive(Debug, Clone)]
pub struct ShutdownHasStarted<T> {
	/// The shutdown reason of the already started shutdown.
	pub reason: T,

	/// The provided reason that was ignored because the shutdown was already started.
	pub ignored: T,
}

impl<T> ShutdownHasStarted<T> {
	pub const fn new(reason: T, ignored_reason: T) -> Self {
		Self {
			reason,
			ignored: ignored_reason,
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

/// Error returned when trying to delay a shutdown that has already been completed.
#[derive(Debug)]
pub struct ShutdownHasCompleted<T> {
	/// The shutdown reason of the already completed shutdown.
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
	use std::{
		future::{self, Future},
		time::Duration,
	};
	use tokio::{
		runtime,
		time::{sleep, timeout},
	};

	use crate::shutdown::{Controller, ShutdownHasCompleted, ShutdownHasStarted};

	// using custom runtime to create non-blocking promises instead of `[tokio::test]`,
	// ensuring predictable asynchronous operations without indefinite blocking
	#[track_caller]
	fn test_runtime(test: impl Future<Output = ()>) {
		let runtime = runtime::Runtime::new().unwrap();
		runtime.block_on(async move {
			let test_with_timeout = timeout(Duration::from_millis(100), test);
			assert!(test_with_timeout.await.is_ok());
		});
	}

	#[test]
	fn shutdown_trigger() {
		// create a `Controller` and instantly trigger a shutdown
		test_runtime(async {
			let controller = Controller::new();
			assert!(controller.trigger_shutdown(1).is_ok());
			assert!(controller.triggered_shutdown().await == 1);
			assert!(controller.completed_shutdown().await == 1);
		});
	}

	#[test]
	fn shutdown_trigger_after_sleep() {
		// use a separate task to trigger a shutdown after a short sleep
		test_runtime(async {
			let controller = Controller::new();

			tokio::spawn({
				let controller = controller.clone();
				async move {
					// sleep, but don't overdue it
					// must not be longer than 100 millis
					sleep(Duration::from_millis(20)).await;
					assert!(controller.trigger_shutdown(22).is_ok());
				}
			});

			assert!(controller.triggered_shutdown().await == 22);
			assert!(controller.completed_shutdown().await == 22);
		});
	}

	#[test]
	fn shutdown_only_once() {
		let controller = Controller::new();

		assert!(controller
			.trigger_shutdown("haven't finished planning yet")
			.is_ok());

		let res = controller.trigger_shutdown("that's not my job");
		if let Err(ShutdownHasStarted { reason, ignored }) = res {
			assert_eq!(reason, "haven't finished planning yet");
			assert_eq!(ignored, "that's not my job");
		} else {
			panic!("Expected ShutdownHasStarted error");
		}
	}

	#[test]
	fn delay_token_shutdown() {
		// postpone shutdown completion using a delay token
		test_runtime(async {
			let controller = Controller::new();
			let token = controller.delay_token().unwrap();

			assert!(controller.trigger_shutdown(1).is_ok());
			controller.triggered_shutdown().await;
			assert!(!controller.is_shutdown_completed());

			tokio::spawn(async move {
				sleep(Duration::from_millis(10)).await;
				drop(token);
			});

			controller.completed_shutdown().await;
		});
	}

	#[test]
	fn shutdown_with_delay() {
		// spawn a future that delays shutdown as long as it is running
		test_runtime(async {
			let controller = Controller::new();
			let token = controller.delay_token().unwrap();
			assert!(controller.trigger_shutdown(1).is_ok());

			tokio::spawn(token.with_future(async move {
				sleep(Duration::from_millis(10)).await;
			}));

			controller.triggered_shutdown().await;
			controller.completed_shutdown().await;
		});
	}

	#[test]
	fn shutdown_completed_from_other_tasks() {
		test_runtime(async {
			let controller = Controller::new();

			tokio::spawn({
				let controller = controller.clone();
				async move {
					sleep(Duration::from_millis(10)).await;
					assert!(controller.trigger_shutdown("i'm bored").is_ok());
				}
			});

			let t = tokio::spawn({
				let controller = controller.clone();
				async move {
					assert!(controller.completed_shutdown().await == "i'm bored");
				}
			});

			assert!(t.await.is_ok());
		});
	}

	#[test]
	fn creating_delay_token_after_shutdown() {
		// try to get delay token after completed shutdown
		let controller = Controller::new();
		assert!(controller.trigger_shutdown("i'm loose").is_ok());

		if let Err(ShutdownHasCompleted { reason }) = controller.delay_token() {
			assert_eq!(reason, "i'm loose");
		} else {
			panic!("Expected ShutdownHasCompleted error");
		}

		if let Err(ShutdownHasCompleted { reason }) = controller.with_delay(future::pending::<()>())
		{
			assert_eq!(reason, "i'm loose");
		} else {
			panic!("Expected ShutdownHasCompleted error");
		}
	}

	#[test]
	fn shutdown_from_task_no_delay() {
		// await a completed shutdown from another task
		test_runtime(async {
			let controller = Controller::new();

			tokio::spawn({
				let controller = controller.clone();
				async move {
					sleep(Duration::from_millis(10)).await;
					assert!(controller.trigger_shutdown(1).is_ok());
				}
			});

			let t = tokio::spawn({
				let controller = controller.clone();
				async move {
					assert!(controller.completed_shutdown().await == 1);
				}
			});

			assert!(t.await.is_ok());
		});
	}

	#[test]
	fn future_with_cancel() {
		// spawn a never-ending task that is canceled on shutdown
		test_runtime(async {
			let controller = Controller::new();
			let task = tokio::spawn(controller.with_cancel(future::pending::<()>()));

			assert!(controller.trigger_shutdown("oi").is_ok());
			if let Ok(Err(reason)) = task.await {
				assert!(reason == "oi");
			} else {
				panic!("Expected `Err(reason)` error");
			}
		})
	}

	#[test]
	fn future_with_cancel_from_signal() {
		// use [`Signal::with_cancel()`] to wrap a future
		test_runtime(async {
			let controller = Controller::new();
			let shutdown_signal = controller.triggered_shutdown();
			let task = tokio::spawn(shutdown_signal.with_cancel(future::pending::<()>()));

			assert!(controller.trigger_shutdown("oi").is_ok());
			if let Ok(Err(reason)) = task.await {
				assert!(reason == "oi");
			} else {
				panic!("Expected `Err(reason)` error");
			}
		})
	}

	#[test]
	fn future_with_cancel_finishes_without_shutdown() {
		// spawn a task with a `Poll::Ready` future and check if it completes with no triggered shutdowns
		test_runtime(async {
			let controller = Controller::<()>::new();
			let task = tokio::spawn(controller.with_cancel(future::ready("born ready")));

			if let Ok(Ok(val)) = task.await {
				assert!(val == "born ready")
			} else {
				panic!("Expected Ok(Ok(val)) result");
			}
		})
	}

	#[test]
	fn future_with_cancel_completes_after_sleep() {
		// spawn a task with a `Poll::Ready` future
		// and after a short sleep check if it completes with no triggered shutdowns
		test_runtime(async {
			let controller = Controller::<()>::new();
			let task = tokio::spawn(controller.with_cancel(async {
				sleep(Duration::from_millis(10)).await;
				"I have heard the summons"
			}));

			if let Ok(Ok(val)) = task.await {
				assert!(val == "I have heard the summons")
			} else {
				panic!("Expected Ok(Ok(val)) result");
			}
		})
	}

	#[test]
	fn shutdown_with_token() {
		// complete a shutdown by dropping a token
		test_runtime(async {
			let shutdown = Controller::new();

			let token = shutdown.trigger_token("stop! hammer time");
			drop(token);

			assert!(shutdown.triggered_shutdown().await == "stop! hammer time");
			assert!(shutdown.completed_shutdown().await == "stop! hammer time");
		});
	}

	#[test]
	fn shutdown_with_trigger_on_ready_future() {
		// trigger the shutdown with a instantly ready future
		test_runtime(async {
			let shutdown = Controller::new();

			tokio::spawn(shutdown.with_trigger("you shall not pass", future::ready(())));

			assert!(shutdown.triggered_shutdown().await == "you shall not pass");
			assert!(shutdown.completed_shutdown().await == "you shall not pass");
		});
	}
}
