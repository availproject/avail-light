use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The [Controller] struct is employed to manage the shutdown procedure of an
/// application or specific tasks. This is achieved by creating a [Monitor] instance
/// for each task that requires monitoring. Upon invoking [Controller::shutdown], all
/// associated [Monitor] instances receive notification that the shutdown process has commenced.

pub struct Controller {
	/// Used for signaling a cancellation request to one or more tasks that retain a reference.
	cancellation_token: CancellationToken,

	/// Used to determine when all [`Monitor`] instances have been dropped.
	task_tracker: mpsc::Sender<()>,

	/// This channel is used to ascertain when all tasks have completed.
	/// Calling recv() will return once all of the corresponding send halves have been dropped."
	task_waiter: mpsc::Receiver<()>,
}

impl Controller {
	pub fn new() -> Self {
		let (task_tracker, task_waiter) = mpsc::channel::<()>(1);
		Self {
			cancellation_token: CancellationToken::new(),
			task_tracker,
			task_waiter,
		}
	}

	/// Creates a new [`Monitor`] instance that can listen for the shutdown signal.
	pub fn watch(&self) -> Monitor {
		Monitor::new(self.cancellation_token.clone(), self.task_tracker.clone())
	}

	pub async fn shutdown(mut self) {
		// notify all task monitors that shutdown has begun
		self.cancellation_token.cancel();

		// destroy the kept mpsc::Sender so that mpsc::Receiver::recv()
		// will return immediately once all tasks have completed (i.e. dropped their mpsc::Sender)
		drop(self.task_tracker);

		// wait for all tasks to finish
		let _ = self.task_waiter.recv().await;
	}
}

impl Default for Controller {
	fn default() -> Self {
		Self::new()
	}
}

/// A [`Monitor`] observes the shutdown signal from the [`Controller`]
/// instance and tracks its reception status.
///
/// Callers can query whether the shutdown signal has been received or not.

pub struct Monitor {
	/// This represents a cloned reference from the [Controller] instance,
	/// utilized for listening to initiated shutdown signals.
	cancellation_token: CancellationToken,

	/// Implicitly used to help the [`Controller`] instance to understand
	/// when the program has completed shutdown.
	_task_tracker: mpsc::Sender<()>,
}

impl Monitor {
	fn new(cancellation_token: CancellationToken, _task_tracker: mpsc::Sender<()>) -> Self {
		Self {
			cancellation_token,
			_task_tracker,
		}
	}

	/// Returns `true` if the shutdown signal has been received, and
	/// `false` otherwise.
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
