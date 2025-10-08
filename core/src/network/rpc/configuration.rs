use crate::types::duration_millis_format;
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
use tokio_retry::strategy::{jitter, ExponentialBackoff, FibonacciBackoff};
#[cfg(target_arch = "wasm32")]
use web_time::Duration;

pub const LOCAL_ENDPOINT: &str = "http://127.0.0.1:9944";
pub const LOCAL_WS_ENDPOINT: &str = "ws://127.0.0.1:9944";

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct RPCConfig {
	/// WebSocket and HTTP endpoint of full node for subscribing to latest header, etc (default: (ws://127.0.0.1:9944, http://127.0.0.1:9944)).
	pub full_node_ws_http: Vec<(String, String)>,
	/// Set the configuration based on which the retries will be orchestrated, max duration [in seconds] between retries and number of tries.
	/// (default:
	/// fibonacci:
	///     base: 1,
	///     max_delay: 10,
	///     retries: 6,
	/// )
	pub retry: RetryConfig,
}

impl Default for RPCConfig {
	fn default() -> Self {
		Self {
			full_node_ws_http: vec![(LOCAL_WS_ENDPOINT.into(), LOCAL_ENDPOINT.into())],
			retry: RetryConfig::Fibonacci(FibonacciConfig {
				base: 1,
				max_delay: Duration::from_millis(10000),
				retries: 8,
			}),
		}
	}
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum RetryConfig {
	#[serde(rename = "exponential")]
	Exponential(ExponentialConfig),

	#[serde(rename = "fibonacci")]
	Fibonacci(FibonacciConfig),
}

impl IntoIterator for RetryConfig {
	type Item = Duration;
	type IntoIter = std::vec::IntoIter<Self::Item>;

	fn into_iter(self) -> Self::IntoIter {
		match self {
			RetryConfig::Exponential(config) => ExponentialBackoff::from_millis(config.base)
				.factor(1000)
				.max_delay(config.max_delay)
				.map(jitter)
				.take(config.retries)
				.collect::<Vec<Duration>>()
				.into_iter(),
			RetryConfig::Fibonacci(config) => FibonacciBackoff::from_millis(config.base)
				.factor(1000)
				.max_delay(config.max_delay)
				.map(jitter)
				.take(config.retries)
				.collect::<Vec<Duration>>()
				.into_iter(),
		}
	}
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ExponentialConfig {
	pub base: u64,
	#[serde(with = "duration_millis_format")]
	pub max_delay: Duration,
	pub retries: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FibonacciConfig {
	pub base: u64,
	#[serde(with = "duration_millis_format")]
	pub max_delay: Duration,
	pub retries: usize,
}
