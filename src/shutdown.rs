use color_eyre::{eyre::eyre, Result};
use std::sync::Arc;

use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;

/// The [`Controller`] struct manages the shutdown procedure of an application or specific tasks.
/// It achieves this by creating a [`Monitor`] instance for each task that requires monitoring. Upon invoking
/// the shutdown procedure, either directly with the [`Controller::shutdown`] method or via the [`Trigger::shutdown`]
/// method from the instantiated remote trigger, associated [`Monitor`] instances receive notification that the shutdown
/// process has commenced.
pub struct Controller {
	/// Used for signaling a cancellation request to one or more tasks that retain a reference.
	cancellation_token: CancellationToken,

	/// Used to determine when all [`Monitor`] instances have been dropped.
	/// Task tracker is under the thread safe guard.
	task_tracker: Arc<RwLock<Option<broadcast::Sender<()>>>>,

	/// This channel is used to ascertain when all tasks have completed.
	/// Calling recv() will return once all of the corresponding send halves have been dropped."
	task_waiter: broadcast::Receiver<()>,
}

impl Controller {
	/// Instantiates a new [`Controller`] and sets up a `tokio::task` responsible for
	/// destroying the `mpsc::Sender` that was retained from its creation. This action
	/// ensures that `mpsc::Receiver::recv()` will return immediately once all tasks have completed.
	/// (i.e., when they have dropped their `mpsc::Sender`)
	pub fn new() -> Self {
		let (task_tracker, task_waiter) = broadcast::channel::<()>(1);
		let cancellation_token = CancellationToken::new();
		let task_tracker = Arc::new(RwLock::new(Some(task_tracker)));

		let token = cancellation_token.clone();
		let guard = task_tracker.clone();
		tokio::spawn(async move {
			// wait for the token to be cancelled
			let _ = token.cancelled().await;
			// rewrite the guarded task tracker Option with None
			if let Some(task_tracker) = guard.write().await.take() {
				// drop the Sender inside the temporary Option
				drop(task_tracker);
			}
		});

		Self {
			cancellation_token,
			task_tracker,
			task_waiter,
		}
	}

	/// Creates a new [`Monitor`] instance that can listen for the shutdown signal.
	pub async fn watch(&self) -> Result<Monitor> {
		let guard = self.task_tracker.read().await;
		let task_tracker = guard
			.as_ref()
			.ok_or(eyre!("Monitor encountered empty Task Tracker reference"))?;
		Ok(Monitor::new(
			self.cancellation_token.clone(),
			task_tracker.clone(),
		))
	}

	/// Initiates the shutdown process and waits for all associated [`Monitor`] instances to be dropped.
	pub async fn shutdown(&mut self) {
		// notify all task monitors that shutdown has begun
		self.cancellation_token.cancel();

		// wait for all tasks to finish
		let _ = self.task_waiter.recv().await;
	}

	/// Creates a new and initialized [`Trigger`] instance.
	pub async fn remote_trigger(&self) -> Result<Trigger> {
		let guard = self.task_tracker.read().await;
		let task_tracker = guard
			.as_ref()
			.ok_or(eyre!("Trigger encountered empty Task Tracker reference"))?;
		Ok(Trigger {
			cancellation_token: self.cancellation_token.clone(),
			task_waiter: task_tracker.subscribe(),
		})
	}
}

/// A [`Monitor`] observes the shutdown signal from the [`Controller`]
/// instance and tracks its reception status.
///
/// Callers can query whether the shutdown signal has been received or not.
#[derive(Clone)]
pub struct Monitor {
	/// This represents a cloned reference from the [Controller] instance,
	/// utilized for listening to initiated shutdown signals.
	cancellation_token: CancellationToken,

	/// Implicitly used to help the [`Controller`] instance to understand
	/// when the program has completed shutdown.
	_task_tracker: broadcast::Sender<()>,
}

impl Monitor {
	/// Instantiates a new [`Monitor`] instance.
	fn new(cancellation_token: CancellationToken, _task_tracker: broadcast::Sender<()>) -> Self {
		Self {
			cancellation_token,
			_task_tracker,
		}
	}

	/// Returns `true` if the shutdown signal has been received, and `false` otherwise.
	pub fn is_shutdown(&self) -> bool {
		self.cancellation_token.is_cancelled()
	}

	/// Receives shutdown notifications, waiting if required.
	pub async fn canceled(&self) {
		// return immediately if the token has already been canceled,
		// don't await futures
		if self.is_shutdown() {
			return;
		}

		// wait here for requested cancellation
		self.cancellation_token.cancelled().await;
	}
}

/// The [`Trigger`] struct serves as a remote shutdown trigger.
/// Triggers are exclusively created via the [`Controller`] instance and
/// encompass all necessary functionalities to be able to initiate and to await
/// for shutdowns of active [`Monitor`] instances.
///
/// Triggers ensure thread safety.

pub struct Trigger {
	cancellation_token: CancellationToken,
	task_waiter: broadcast::Receiver<()>,
}

impl Trigger {
	/// Initiates the shutdown process and waits for all associated [`Monitor`] instances to be dropped.
	pub async fn shutdown(mut self) {
		// don't wait for futures, return immediately if the token has been canceled
		if self.cancellation_token.is_cancelled() {
			return;
		}

		// notify all task monitors that shutdown has begun
		self.cancellation_token.cancel();

		// wait for all tasks to finish
		let _ = self.task_waiter.recv().await;
	}
}

#[cfg(test)]
mod tests {
	use crate::shutdown::Controller;
	use color_eyre::Result;

	#[tokio::test]
	async fn shutdown_ends() -> Result<()> {
		let mut shutdown = Controller::new();

		let t = tokio::spawn({
			let monitor = shutdown.watch().await?;
			async move {
				monitor.canceled().await;
			}
		});

		shutdown.shutdown().await;
		assert!(t.await.is_ok());

		Ok(())
	}

	#[tokio::test]
	async fn monitor_is_shutdown() -> Result<()> {
		let mut shutdown = Controller::new();

		let t = tokio::spawn({
			let monitor = shutdown.watch().await?;
			async move {
				assert!(monitor.is_shutdown());
			}
		});

		shutdown.shutdown().await;
		assert!(t.await.is_ok());

		Ok(())
	}

	#[tokio::test]
	async fn monitor_cloned() -> Result<()> {
		let mut shutdown = Controller::new();
		let monitor = shutdown.watch().await?;
		let cloned_monitor = monitor.clone();

		let t1 = tokio::spawn({
			async move {
				assert!(monitor.is_shutdown());
			}
		});

		let t2 = tokio::spawn({
			async move {
				assert!(cloned_monitor.is_shutdown());
			}
		});

		shutdown.shutdown().await;
		assert!(t1.await.is_ok());
		assert!(t2.await.is_ok());

		Ok(())
	}

	#[tokio::test]
	async fn canceled_is_idempotent() -> Result<()> {
		let mut shutdown = Controller::new();

		let t = tokio::spawn({
			let monitor = shutdown.watch().await?;
			async move {
				monitor.canceled().await;
				monitor.canceled().await;
			}
		});

		shutdown.shutdown().await;
		assert!(t.await.is_ok());

		Ok(())
	}

	#[tokio::test]
	async fn watch_error() -> Result<()> {
		let mut shutdown = Controller::new();

		tokio::spawn({
			let monitor = shutdown.watch().await?;
			async move {
				monitor.canceled().await;
			}
		});

		shutdown.shutdown().await;
		// should be error, right here
		assert!(shutdown.watch().await.is_err());

		Ok(())
	}

	#[tokio::test]
	async fn shutdown_with_trigger() -> Result<()> {
		let shutdown = Controller::new();
		let trigger = shutdown.remote_trigger().await?;

		let t = tokio::spawn({
			let monitor = shutdown.watch().await?;
			async move {
				monitor.canceled().await;
			}
		});

		trigger.shutdown().await;
		assert!(t.await.is_ok());

		Ok(())
	}

	#[tokio::test]
	async fn trigger_is_idempotent() -> Result<()> {
		let shutdown = Controller::new();
		let trigger1 = shutdown.remote_trigger().await?;
		let trigger2 = shutdown.remote_trigger().await?;

		let t = tokio::spawn({
			let monitor = shutdown.watch().await?;
			async move {
				monitor.canceled().await;
			}
		});

		trigger1.shutdown().await;
		trigger2.shutdown().await;
		assert!(t.await.is_ok());

		Ok(())
	}

	#[tokio::test]
	async fn trigger_error() -> Result<()> {
		let shutdown = Controller::new();
		let trigger = shutdown.remote_trigger().await?;

		let t = tokio::spawn({
			let monitor = shutdown.watch().await?;
			async move {
				monitor.canceled().await;
			}
		});

		trigger.shutdown().await;
		assert!(t.await.is_ok());
		// should be error, right here
		assert!(shutdown.remote_trigger().await.is_err());

		Ok(())
	}
}
