extern crate rocksdb;

use std::{
	convert::TryInto,
	pin::Pin,
	sync::{mpsc::SyncSender, Arc},
	task::{Context, Poll},
};

use ::futures::prelude::*;
use anyhow::Result;
use chrono::{DateTime, Local};
use hyper::{
	header::ACCESS_CONTROL_ALLOW_ORIGIN, service::Service, Body, Method, Request, Response, Server,
	StatusCode,
};
use num::{BigUint, FromPrimitive};
use regex::Regex;
use rocksdb::DB;

use crate::types::CellContentQueryPayload;

pub fn calculate_confidence(count: u32) -> f64 { 100f64 * (1f64 - 1f64 / 2u32.pow(count) as f64) }

pub fn serialised_confidence(block: u64, factor: f64) -> Option<String> {
	let block_big: BigUint = FromPrimitive::from_u64(block)?;
	let factor_big: BigUint = FromPrimitive::from_u64((10f64.powi(7) * factor) as u64)?;
	let shifted: BigUint = block_big << 32 | factor_big;
	Some(shifted.to_str_radix(10))
}

// 💡 HTTP part where handles the RPC Queries

//service part of hyper
struct Handler {
	store: Arc<DB>,
}

impl Service<Request<Body>> for Handler {
	type Error = hyper::Error;
	type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
	type Response = Response<Body>;

	fn poll_ready(&mut self, _: &mut Context) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, req: Request<Body>) -> Self::Future {
		fn match_url(path: &str) -> Result<u64, String> {
			let re = Regex::new(r"^(/v1/confidence/(\d{1,}))$").expect("valid regex");
			re.captures(path)
				.and_then(|caps| caps.get(2))
				.ok_or_else(|| "no match found!".to_string())
				.and_then(|block| {
					block
						.as_str()
						.parse::<u64>()
						.map_err(|error| format!("cannot parse path: {error}"))
				})
		}

		fn mk_response(s: String) -> Result<Response<Body>, hyper::Error> {
			Ok(Response::builder()
				.status(200)
				.header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
				.header("Content-Type", "application/json")
				.body(Body::from(s))
				.expect("a valid body"))
		}

		fn get_confidence(db: Arc<DB>, block: u64) -> Result<u32, String> {
			let cf_handle = db
				.cf_handle(crate::consts::CONFIDENCE_FACTOR_CF)
				.ok_or_else(|| "cannot find column family".to_string())?;

			db.get_cf(&cf_handle, block.to_be_bytes())
				.map_err(|error| format!("failed to find entry in confidence store: {error}"))
				.and_then(|found| found.ok_or_else(|| "no entry in confidence store".to_string()))
				.and_then(|confidence| {
					confidence
						.try_into()
						.map_err(|_| "fail to cast confidence to 4 bytes".to_string())
				})
				.map(u32::from_be_bytes)
		}

		let local_tm: DateTime<Local> = Local::now();
		log::info!(
			"⚡️ {} | {} | {}",
			local_tm.to_rfc2822(),
			req.method(),
			req.uri().path()
		);

		let db = self.store.clone();

		Box::pin(async move {
			let res = match req.method() {
				&Method::GET => {
					if let Ok(block_num) = match_url(req.uri().path()) {
						let count = match get_confidence(db.clone(), block_num) {
							Ok(count) => {
								log::info!("Confidence for block {} found in a store", block_num);
								count
							},
							Err(e) => {
								// if for some reason confidence is not found
								// in on disk database, client receives following response
								log::info!("error: {}", e);
								0
							},
						};
						let conf = calculate_confidence(count);
						let serialised_conf = match serialised_confidence(block_num, conf) {
							Some(value) => value,
							None => {
								log::error!("Cannot serialise confidence due to invalid block {} or confidence {}.", block_num, conf);
								"0".to_string()
							},
						};

						mk_response(format!(
							r#"{{"block": {}, "confidence": {}, "serialisedConfidence": {}}}"#,
							block_num, conf, serialised_conf
						))
					} else {
						let mut not_found = Response::default();
						*not_found.status_mut() = StatusCode::NOT_FOUND;
						not_found.headers_mut().insert(
							ACCESS_CONTROL_ALLOW_ORIGIN,
							"*".parse().expect("* to be parsable"),
						);
						Ok(not_found)
					}
				},
				_ => {
					let mut not_found = Response::default();
					*not_found.status_mut() = StatusCode::NOT_FOUND;
					not_found.headers_mut().insert(
						ACCESS_CONTROL_ALLOW_ORIGIN,
						"*".parse().expect("* to be parsable"),
					);
					Ok(not_found)
				},
			};
			res
		})
	}
}

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
		let store = self.store.clone();
		let fut = async move { Ok(Handler { store }) };
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
		.map_err(|_| "Bad Http server host/ port, found in config file")?;
	let server = Server::bind(&addr).serve(MakeHandler { store });

	log::info!(
		"RPC running on http://{}:{}",
		cfg.http_server_host,
		cfg.http_server_port
	);

	server.await?;
	Ok(())
}
