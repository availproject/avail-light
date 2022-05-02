use std::{
	pin::Pin,
	sync::{mpsc::SyncSender, Arc},
	task::{Context, Poll},
};

use ::futures::prelude::*;
use chrono::Local;
use derive_more::From;
use hyper::{
	header::ACCESS_CONTROL_ALLOW_ORIGIN, service::Service, Body, Method, Request, Response, Server,
	StatusCode, Uri,
};
use num::{BigUint, FromPrimitive};
use regex::Regex;
use rocksdb::DB;

use crate::{
	consts::CONFIDENCE_FACTOR_CF,
	error::{self, into_error, into_warning},
	types::CellContentQueryPayload,
};

pub fn calculate_confidence(count: u32) -> f64 { 100f64 * (1f64 - 1f64 / 2u32.pow(count) as f64) }

pub fn serialised_confidence(block: u64, factor: f64) -> String {
	let _block: BigUint = FromPrimitive::from_u64(block).unwrap();
	let _factor: BigUint = FromPrimitive::from_u64((10f64.powi(7) * factor) as u64).unwrap();
	let _shifted: BigUint = _block << 32 | _factor;
	_shifted.to_str_radix(10)
}

// 💡 HTTP part where handles the RPC Queries

//service part of hyper
#[derive(From)]
struct Handler {
	store: Arc<DB>,
}

impl Handler {
	/// Gets the block ID from `url`.
	fn get_block_from_url(url: &str) -> Option<u64> {
		let re = Regex::new(r"^(/v1/confidence/(\d{1,}))$").expect("URL regex is valid .qed");

		let block_match = re.captures(url).and_then(|cap| cap.get(2))?;
		block_match.as_str().parse::<u64>().ok()
	}

	/// Creates a HTTP response (`status=200`) with a
	/// # TODO
	/// - **Security**: We need a fine grain tune on ALLOW ORIGIN.
	fn build_response<S: Into<Body>>(body: S) -> Result<Response<Body>, error::App> {
		Response::builder()
			.status(200)
			.header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
			.header("Content-Type", "application/json")
			.body(body.into())
			.map_err(|err| error::HttpServer::from(err).into())
	}

	/// Returns the confidence of the given `block`.
	fn get_confidence(store: &DB, block: u64) -> Result<u32, error::App> {
		let cf = store
			.cf_handle(CONFIDENCE_FACTOR_CF)
			.expect("`CONFIDENCE_FACTOR_CF` is valid .qed");

		let encoded_confident = store
			.get_cf(&cf, block.to_be_bytes())?
			.ok_or(error::HttpServer::MissingConfident(block))?;

		<[u8; 4]>::try_from(encoded_confident)
			.map_err(|_| error::HttpServer::InvalidConfident.into())
			.map(u32::from_be_bytes)
	}

	/// It processes a HTTP request for the confidence of a block (defined at the `url`.
	fn do_http_get_method(store: &DB, url: &Uri) -> Result<Response<Body>, error::App> {
		let block_num =
			Self::get_block_from_url(url.path()).ok_or(error::HttpServer::InvalidUrl)?;

		let count = Self::get_confidence(store, block_num)
			.inspect_err(into_error)
			.inspect(|count| {
				log::info!(
					"Confidence {} for block {} found in a store",
					count,
					block_num
				)
			})
			.ok()
			.unwrap_or_default();

		let conf = calculate_confidence(count);
		let serialised_conf = serialised_confidence(block_num, conf);
		Self::build_response(format!(
			r#"{{"block": {}, "confidence": {}, "serialisedConfidence": {}}}"#,
			block_num, conf, serialised_conf
		))
	}

	/// Creates a *NOT_FOUND* response.
	fn not_found_response() -> Response<Body> {
		// let all_cors = "*".parse().expect("The `*` ALLOW ORIGIN is always valid .qed");
		Response::builder()
			.status(StatusCode::NOT_FOUND)
			.header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
			.body(Body::empty())
			.expect("NOT_FOUND valid response .qed")
	}
}

impl Service<Request<Body>> for Handler {
	type Error = hyper::Error;
	type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
	type Response = Response<Body>;

	fn poll_ready(&mut self, _: &mut Context) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, req: Request<Body>) -> Self::Future {
		log::info!(
			"⚡️ {} | {} | {}",
			Local::now().to_rfc2822(),
			req.method(),
			req.uri().path()
		);

		let db = Arc::clone(&self.store);

		let f = async move {
			let res = match req.method() {
				&Method::GET => Self::do_http_get_method(&db, req.uri())
					.inspect_err(into_warning)
					.ok(),
				_ => None,
			};

			let res = res.unwrap_or_else(Self::not_found_response);
			Ok(res)
		};

		Box::pin(f)
	}
}

#[derive(From)]
struct MakeHandler {
	store: Arc<DB>,
}

impl<T> Service<T> for MakeHandler {
	type Error = hyper::Error;
	type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
	type Response = Handler;

	fn poll_ready(&mut self, _: &mut Context) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, _: T) -> Self::Future {
		let handler = Arc::clone(&self.store).into();
		let fut = async move { Ok(handler) };
		Box::pin(fut)
	}
}

#[tokio::main]
pub async fn run_server(
	store: Arc<DB>,
	cfg: super::types::RuntimeConfig,
	_cell_query_tx: SyncSender<CellContentQueryPayload>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
	let addr = format!("{}:{}", cfg.http_server_host, cfg.http_server_port)
		.parse()
		.expect("Bad Http server host/ port, found in config file");
	let store_handler: MakeHandler = store.into();
	let server = Server::bind(&addr).serve(store_handler);

	log::info!(
		"RPC running on http://{}:{}",
		cfg.http_server_host,
		cfg.http_server_port
	);

	server.await?;
	Ok(())
}
