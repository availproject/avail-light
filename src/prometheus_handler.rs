use std::net::SocketAddr;

use anyhow::{Context, Error, Result};
use hyper::{
	http::StatusCode,
	server::Server,
	service::{make_service_fn, service_fn},
	Body, Request, Response,
};
pub use prometheus::{
	self,
	core::{
		AtomicF64 as F64, AtomicI64 as I64, AtomicU64 as U64, GenericCounter as Counter,
		GenericCounterVec as CounterVec, GenericGauge as Gauge, GenericGaugeVec as GaugeVec,
	},
	exponential_buckets, Error as PrometheusError, Histogram, HistogramOpts, HistogramVec, Opts,
	Registry,
};
use prometheus::{core::Collector, Encoder, TextEncoder};

pub fn register<T: Clone + Collector + 'static>(
	metric: T,
	registry: &Registry,
) -> Result<T, PrometheusError> {
	registry.register(Box::new(metric.clone()))?;
	Ok(metric)
}

async fn request_metrics(req: Request<Body>, registry: Registry) -> Result<Response<Body>> {
	if req.uri().path() == "/metrics" {
		let metric_families = registry.gather();
		let mut buffer = vec![];
		let encoder = TextEncoder::new();
		encoder
			.encode(&metric_families, &mut buffer)
			.context("Metric encoding error");

		Response::builder()
			.status(StatusCode::OK)
			.header("Content-Type", encoder.format_type())
			.body(Body::from(buffer))
			.map_err(|e| anyhow::anyhow!("Error creating body from buffer: {}", e))
	} else {
		Response::builder()
			.status(StatusCode::NOT_FOUND)
			.body(Body::from("Not found."))
			.map_err(|_| anyhow::anyhow!("Endpoint not found"))
	}
}

/// Init prometheus using the given listener.
pub async fn init_prometheus_with_listener(
	prometheus_addr: SocketAddr,
	registry: Registry,
) -> Result<(), Error> {
	let listener = tokio::net::TcpListener::bind(&prometheus_addr)
		.await
		.map_err(|_| anyhow::anyhow!("Port already in use: {}", prometheus_addr))?;

	let listener = hyper::server::conn::AddrIncoming::from_listener(listener)?;
	log::info!("Prometheus exporter started at {}", listener.local_addr());

	let service = make_service_fn(move |_| {
		let registry = registry.clone();

		async move {
			Ok::<_, hyper::Error>(service_fn(move |req: Request<Body>| {
				request_metrics(req, registry.clone())
			}))
		}
	});

	let server = Server::builder(listener).serve(service);

	let result = server.await.map_err(Into::into);

	result
}
