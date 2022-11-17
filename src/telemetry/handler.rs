use std::sync::Arc;

use anyhow::Result;

use hyper::{Body, Method, Request, Response, StatusCode};
use prometheus_client::{encoding::text::encode, registry::Registry};
use tokio::sync::Mutex;

type SharedRegistry = Arc<Mutex<Registry>>;

pub async fn new(ctx: SharedRegistry, req: Request<Body>) -> Result<Response<Body>> {
	match (req.method(), req.uri().path()) {
		(&Method::GET, "/metrics") => respond_with_metrics(ctx).await,
		_ => not_found(),
	}
}

async fn respond_with_metrics(reg: SharedRegistry) -> Result<Response<Body>> {
	let mut encoded: Vec<u8> = Vec::new();
	let reg = reg.lock().await;
	encode(&mut encoded, &reg)?;

	let metrics_content_type = "application/openmetrics-text;charset=utf-8;version=1.0.0";
	let res = Response::builder()
		.status(StatusCode::OK)
		.header(hyper::header::CONTENT_TYPE, metrics_content_type)
		.body(Body::from(encoded))?;

	Ok(res)
}

// HTTP status code 404
fn not_found() -> Result<Response<Body>> {
	let res = Response::builder()
		.status(StatusCode::NOT_FOUND)
		.body(Body::from("Not found. Try localhost:[port]/metrics"))?;
	Ok(res)
}
