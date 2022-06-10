extern crate rocksdb;

use std::{
	convert::TryInto,
	pin::Pin,
	sync::{mpsc::SyncSender, Arc},
	task::{Context, Poll},
};

use ::futures::prelude::*;
use chrono::{DateTime, Local};
use hyper::{
	header::ACCESS_CONTROL_ALLOW_ORIGIN, service::Service, Body, Method, Request, Response, Server,
	StatusCode,
};
use num::{BigUint, FromPrimitive};
use regex::Regex;
use rocksdb::{BoundColumnFamily, DB};

use crate::types::{CellContentQueryPayload, RuntimeConfig};

pub fn calculate_confidence(count: u32) -> f64 { 100f64 * (1f64 - 1f64 / 2u32.pow(count) as f64) }

pub fn serialised_confidence(block: u64, factor: f64) -> String {
	let _block: BigUint = FromPrimitive::from_u64(block).unwrap();
	let _factor: BigUint = FromPrimitive::from_u64((10f64.powi(7) * factor) as u64).unwrap();
	let _shifted: BigUint = _block << 32 | _factor;
	_shifted.to_str_radix(10)
}

// 💡 HTTP part where handles the RPC Queries

//service part of hyper
struct Handler {
	store: Arc<DB>,
	cfg: RuntimeConfig,
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
			let re = Regex::new(r"^(/v1/confidence/(\d{1,}))$").unwrap();
			if let Some(caps) = re.captures(path) {
				if let Some(block) = caps.get(2) {
					return Ok(block.as_str().parse::<u64>().unwrap());
				}
			}
			Err("no match found !".to_owned())
		}
		fn match_app_url(path: &str) -> Result<u64, String> {
			let re = Regex::new(r"^(/v1/appdata/(\d{1,}))$").unwrap();
			if let Some(caps) = re.captures(path) {
				if let Some(block) = caps.get(2) {
					return Ok(block.as_str().parse::<u64>().unwrap());
				}
			}
			Err("no match found !".to_owned())
		}

		fn mk_response(s: String) -> Result<Response<Body>, hyper::Error> {
			Ok(Response::builder()
				.status(200)
				.header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
				.header("Content-Type", "application/json")
				.body(Body::from(s))
				.unwrap())
		}

		fn get_confidence(
			db: Arc<DB>,
			cf_handle: Arc<BoundColumnFamily<'_>>,
			block: u64,
		) -> Result<u32, String> {
			match db.get_cf(&cf_handle, block.to_be_bytes()) {
				Ok(v) => match v {
					Some(v) => Ok(u32::from_be_bytes(v.try_into().unwrap())),
					None => Err("failed to find entry in confidence store".to_owned()),
				},
				Err(_) => Err("failed to find entry in confidence store".to_owned()),
			}
		}

		fn get_data(
			db: Arc<DB>,
			cf_handle: Arc<BoundColumnFamily<'_>>,
			app_id: u32,
			block: u64,
		) -> Result<Vec<Vec<u8>>, String> {
			let key = format!("{}:{}", app_id, block);
			// let x =db.get_cf(&cf_handle, key.as_bytes()).unwrap().unwrap();
			match db.get_cf(&cf_handle, key.as_bytes()) {
				Ok(v) => match v {
					Some(v) => Ok(serde_json::from_slice(&v).unwrap()),
					None => Err("failed to find entry in data store".to_owned()),
				},
				Err(_) => Err("failed to find entry in data store".to_owned()),
			}
		}

		let local_tm: DateTime<Local> = Local::now();
		log::info!(
			"⚡️ {} | {} | {}",
			local_tm.to_rfc2822(),
			req.method(),
			req.uri().path()
		);

		let db = self.store.clone();
		let cfg = self.cfg.clone();

		Box::pin(async move {
			let res = match req.method() {
				&Method::GET => {
					let path = req.uri().path();
					let v: Vec<&str> = path.split('/').collect();
					match v[2] {
						"confidence" => {
							let block_num = match_url(path).unwrap();
							let count = match get_confidence(
								db.clone(),
								db.cf_handle(crate::consts::CONFIDENCE_FACTOR_CF).unwrap(),
								block_num,
							) {
								Ok(count) => {
									log::info!(
										"Confidence for block {} found in a store",
										block_num
									);
									count
								},
								Err(e) => {
									// if for some reason confidence is not found
									// in on disk database, client receives following response
									log::info!("error: {}", e);
									0
								},
							};
							let confidence = calculate_confidence(count);
							let serialised_conf = serialised_confidence(block_num, confidence);
							mk_response(
								format!(
									r#"{{"block": {}, "confidence": {}, "serialisedConfidence": {}}}"#,
									block_num, confidence, serialised_conf
								)
								.to_owned(),
							)
						},
						"appdata" => {
							let block = match_app_url(path).unwrap();
							let app_id = cfg.app_id.unwrap();
							println!("fetching data for app_id: {}", app_id);
							let data = match get_data(
								db.clone(),
								db.cf_handle(crate::consts::APP_DATA_CF).unwrap(),
								app_id,
								block,
							) {
								Ok(data) => {
									log::info!("Data for block {} found in a store", block);
									data
								},
								Err(e) => {
									// if for some reason data is not found
									// in on disk database, client receives following response
									log::info!("error: {}", e);
									vec![]
								},
							};
							let data_hex_string = data
								.iter()
								.map(|e| {
									e.iter().fold(String::new(), |mut acc, e| {
										acc.push_str(format!("{:02x}", e).as_str());
										acc
									})
								})
								.collect::<Vec<_>>();

							mk_response(
								format!(
									r#"{{"block": {}, "app_data": {:?}}}"#,
									block, data_hex_string
								)
								.to_owned(),
							)
						},
						_ => {
							let mut not_found = Response::default();
							*not_found.status_mut() = StatusCode::NOT_FOUND;
							not_found
								.headers_mut()
								.insert(ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
							Ok(not_found)
						},
					}
				},
				_ => {
					let mut not_found = Response::default();
					*not_found.status_mut() = StatusCode::NOT_FOUND;
					not_found
						.headers_mut()
						.insert(ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
					Ok(not_found)
				},
			};
			res
		})
	}
}

struct MakeHandler {
	store: Arc<DB>,
	cfg: RuntimeConfig,
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
		let cfg = self.cfg.clone();
		let fut = async move { Ok(Handler { store, cfg }) };
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
	let cfg_clone = cfg.clone();
	let server = Server::bind(&addr).serve(MakeHandler { store, cfg });

	log::info!(
		"RPC running on http://{}:{}",
		cfg_clone.http_server_host,
		cfg_clone.http_server_port
	);

	server.await?;
	Ok(())
}
