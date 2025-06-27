use color_eyre::Result;
use opentelemetry::{
	global,
	metrics::{Counter, Gauge, MeterProvider},
	KeyValue,
};
use opentelemetry_otlp::{MetricExporter, Protocol, WithExportConfig};
use opentelemetry_sdk::{
	metrics::{PeriodicReader, SdkMeterProvider},
	runtime,
};
use std::{
	collections::HashMap,
	sync::{Arc, Mutex},
	time::Duration,
};
use tracing::info;

#[derive(Clone)]
pub struct MonitorMetrics {
	active_peers: Gauge<u64>,
	blocked_peers: Gauge<u64>,
	discovered_peers: Counter<u64>,
	bootstrap_attempts: Counter<u64>,
	bootstrap_failures: Counter<u64>,
	// These hashmaps are for caching gauge instances to avoid recreating them on every metric update
	peer_ping_latency: Arc<Mutex<HashMap<String, Gauge<f64>>>>,
	peer_health_score: Arc<Mutex<HashMap<String, Gauge<f64>>>>,
	peer_blocked_status: Arc<Mutex<HashMap<String, Gauge<f64>>>>,
	meter_provider: SdkMeterProvider,
}

impl MonitorMetrics {
	pub fn new(
		endpoint: String,
		export_period_secs: u64,
		export_timeout_secs: u64,
	) -> Result<Self> {
		info!("Initializing monitor metrics with endpoint: {}", endpoint);

		let exporter = MetricExporter::builder()
			.with_tonic()
			.with_endpoint(&endpoint)
			.with_protocol(Protocol::Grpc)
			.with_timeout(Duration::from_secs(export_timeout_secs))
			.build()?;

		let reader = PeriodicReader::builder(exporter, runtime::Tokio)
			.with_interval(Duration::from_secs(export_period_secs))
			.build();

		let meter_provider = SdkMeterProvider::builder().with_reader(reader).build();

		global::set_meter_provider(meter_provider.clone());

		let meter = meter_provider.meter("avail_light_monitor");

		let active_peers = meter
			.u64_gauge("monitor.active_peers")
			.with_description("Number of currently active peers")
			.build();

		let blocked_peers = meter
			.u64_gauge("monitor.blocked_peers")
			.with_description("Number of blocked peers")
			.build();

		let discovered_peers = meter
			.u64_counter("monitor.discovered_peers")
			.with_description("Total number of peers discovered")
			.build();

		let bootstrap_attempts = meter
			.u64_counter("monitor.bootstrap_attempts")
			.with_description("Total number of bootstrap attempts")
			.build();

		let bootstrap_failures = meter
			.u64_counter("monitor.bootstrap_failures")
			.with_description("Total number of bootstrap failures")
			.build();

		Ok(Self {
			active_peers,
			blocked_peers,
			discovered_peers,
			bootstrap_attempts,
			bootstrap_failures,
			peer_ping_latency: Arc::new(Mutex::new(HashMap::new())),
			peer_health_score: Arc::new(Mutex::new(HashMap::new())),
			peer_blocked_status: Arc::new(Mutex::new(HashMap::new())),
			meter_provider,
		})
	}

	pub fn set_active_peers(&self, count: u64) {
		self.active_peers.record(count, &[]);
	}

	pub fn set_blocked_peers(&self, count: u64) {
		self.blocked_peers.record(count, &[]);
	}

	pub fn inc_discovered_peers(&self, count: u64) {
		self.discovered_peers.add(count, &[]);
	}

	pub fn inc_bootstrap_attempts(&self) {
		self.bootstrap_attempts.add(1, &[]);
	}

	pub fn inc_bootstrap_failures(&self) {
		self.bootstrap_failures.add(1, &[]);
	}

	// Per-peer metrics
	pub fn set_peer_ping_latency(&self, peer_id: &str, latency_ms: f64) {
		let mut latencies = self.peer_ping_latency.lock().unwrap();

		if let Some(gauge) = latencies.get(peer_id) {
			gauge.record(latency_ms, &[KeyValue::new("peer_id", peer_id.to_string())]);
		} else {
			let meter = self.meter_provider.meter("avail_light_monitor");
			let gauge = meter
				.f64_gauge("monitor.peer_ping_latency_ms")
				.with_description("Ping latency to peer in milliseconds")
				.build();

			gauge.record(latency_ms, &[KeyValue::new("peer_id", peer_id.to_string())]);
			latencies.insert(peer_id.to_string(), gauge);
		}
	}

	pub fn set_peer_health_score(&self, peer_id: &str, score: f64) {
		let mut scores = self.peer_health_score.lock().unwrap();

		if let Some(gauge) = scores.get(peer_id) {
			gauge.record(score, &[KeyValue::new("peer_id", peer_id.to_string())]);
		} else {
			let meter = self.meter_provider.meter("avail_light_monitor");
			let gauge = meter
				.f64_gauge("monitor.peer_health_score")
				.with_description("Health score of peer (0-100)")
				.build();

			gauge.record(score, &[KeyValue::new("peer_id", peer_id.to_string())]);
			scores.insert(peer_id.to_string(), gauge);
		}
	}

	pub fn set_peer_blocked_status(&self, peer_id: &str, is_blocked: bool) {
		let mut blocked = self.peer_blocked_status.lock().unwrap();
		let value = if is_blocked { 1.0 } else { 0.0 };

		if let Some(gauge) = blocked.get(peer_id) {
			gauge.record(value, &[KeyValue::new("peer_id", peer_id.to_string())]);
		} else {
			let meter = self.meter_provider.meter("avail_light_monitor");
			let gauge = meter
				.f64_gauge("monitor.peer_blocked")
				.with_description("Whether peer is blocked (1) or not (0)")
				.build();

			gauge.record(value, &[KeyValue::new("peer_id", peer_id.to_string())]);
			blocked.insert(peer_id.to_string(), gauge);
		}
	}
}
