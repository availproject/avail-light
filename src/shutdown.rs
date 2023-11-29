use std::{
	fmt::Debug,
	mem,
	sync::{Arc, Mutex},
	task::Waker,
};

pub struct Controller<T: Clone> {
	inner: Arc<Mutex<ControllerInner<T>>>,
}

impl<T: Clone> Controller<T> {
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
}

struct ControllerInner<T> {
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

impl<T: Clone> Default for Controller<T> {
	fn default() -> Self {
		Self::new()
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
