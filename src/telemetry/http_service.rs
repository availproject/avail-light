use std::{
	pin::Pin,
	sync::{Arc, Mutex},
	task::{Context, Poll},
};

use anyhow::{Error, Result};
use futures::Future;
use hyper::{service::Service, Body, Method, Request, Response, StatusCode};
use prometheus_client::{encoding::text::encode, registry::Registry};

type SharedRegistry = Arc<Mutex<Registry>>;

pub struct MetricService {
	reg: SharedRegistry,
}

impl MetricService {
	fn get_reg(&mut self) -> SharedRegistry {
		Arc::clone(&self.reg)
	}

	fn respond_with_metrics(&mut self) -> Result<Response<Body>> {
		let mut encoded: Vec<u8> = Vec::new();
		let reg_mutex = self.get_reg();

		let reg = match reg_mutex.lock() {
			Ok(reg) => reg,
			Err(_) => return Err(Error::msg("Could not acquire lock on Prometheus Registry")),
		};

		encode(&mut encoded, &reg)?;
		let metrics_content_type = "application/openmetrics-text;charset=utf-8;version=1.0.0";
		let res = Response::builder()
			.status(StatusCode::OK)
			.header(hyper::header::CONTENT_TYPE, metrics_content_type)
			.body(Body::from(encoded))?;
		Ok(res)
	}

	fn respond_with_404_not_found(&mut self) -> Result<Response<Body>> {
		let res = Response::builder()
			.status(StatusCode::NOT_FOUND)
			.body(Body::from("Not found. Try localhost:[port]/metrics"))?;
		Ok(res)
	}
}

impl Service<Request<Body>> for MetricService {
	type Response = Response<Body>;
	type Error = anyhow::Error;
	type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

	fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, req: Request<Body>) -> Self::Future {
		let req_path = req.uri().path();
		let req_method = req.method();
		let resp = if (req_method == Method::GET) && (req_path == "/metrics") {
			self.respond_with_metrics()
		} else {
			self.respond_with_404_not_found()
		};

		match resp {
			Ok(res) => return Box::pin(async { Ok(res) }),
			Err(err) => return Box::pin(async { Err(err) }),
		}
	}
}

pub struct MakeMetricService {
	reg: SharedRegistry,
}

impl MakeMetricService {
	pub fn new(registry: Registry) -> MakeMetricService {
		MakeMetricService {
			reg: Arc::new(Mutex::new(registry)),
		}
	}
}

impl<T> Service<T> for MakeMetricService {
	type Response = MetricService;
	type Error = hyper::Error;
	type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

	fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, _: T) -> Self::Future {
		let reg = self.reg.clone();
		let fut = async move { Ok(MetricService { reg }) };
		Box::pin(fut)
	}
}
