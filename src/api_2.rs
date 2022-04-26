extern crate rocksdb;

use std::{
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use ::futures::prelude::*;
use chrono::{DateTime, Local};
use hyper::{
	header::ACCESS_CONTROL_ALLOW_ORIGIN, service::Service, Body, Method, Request, Response, Server,
	StatusCode,
};
use regex::Regex;
use rocksdb::{DBWithThreadMode, SingleThreaded};
use tokio;

use crate::{
	rpc::{
		check_http, generate_app_specific_cells, get_block_by_number, get_kate_query_proof_by_cell,
	},
	types::{Block, Cell, Header},
};

// 💡 HTTP part where handles the RPC Queries

//service part of hyper
struct Handler {
	store: Arc<DBWithThreadMode<SingleThreaded>>,
	url: String,
}

impl Service<Request<Body>> for Handler {
	type Error = hyper::Error;
	type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
	type Response = Response<Body>;

	fn poll_ready(&mut self, _: &mut Context) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, req: Request<Body>) -> Self::Future {
		fn match_url_block(path: &str) -> Result<u64, String> {
			let re = Regex::new(r"^(/v1/appdata/(\d{1,})/(\d{1,}))$").unwrap();
			if let Some(caps) = re.captures(path) {
				if let Some(block) = caps.get(2) {
					return Ok(block.as_str().parse::<u64>().unwrap());
				}
			}
			Err("no match found !".to_owned())
		}
		fn match_url_id(path: &str) -> Result<u64, String> {
			let re = Regex::new(r"^(/v1/appdata/(\d{1,})/(\d{1,}))$").unwrap();
			if let Some(caps) = re.captures(path) {
				if let Some(block) = caps.get(3) {
					return Ok(block.as_str().parse::<u64>().unwrap());
				}
			}
			Err("no match found !".to_owned())
		}
		fn check_id(
			header: Header,
			app_id: u32,
			block_num: u64,
			block: Block,
		) -> Result<Vec<Cell>, String> {
			let max_cols = header.extrinsics_root.cols;
			let index = header.app_data_lookup.index;
			let cells = match index
				.iter()
				.find(|elem| app_id != 0 && app_id as u32 == elem.0)
			{
				Some((app_id, offset)) => {
					log::info!(
						"{} chunks for app {} found in block {}",
						offset,
						app_id,
						block_num
					);
					generate_app_specific_cells(*offset, max_cols, block_num, block, *app_id)
				},
				None => {
					vec![]
				},
			};
			Ok(cells)
		}

		fn mk_response(s: String) -> Result<Response<Body>, hyper::Error> {
			Ok(Response::builder()
				.status(200)
				.header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
				.header("Content-Type", "application/json")
				.body(Body::from(s))
				.unwrap())
		}

		let local_tm: DateTime<Local> = Local::now();
		log::info!(
			"⚡️ {} | {} | {}",
			local_tm.to_rfc2822(),
			req.method(),
			req.uri().path()
		);

		let _db = self.store.clone();
		let url = self.url.clone();

		Box::pin(async move {
			let res = match req.method() {
				&Method::GET => {
					if let Ok(block_num) = match_url_block(req.uri().path()) {
						if let Ok(app_id) = match_url_id(req.uri().path()) {
							//cell logic to be written here
							let block = get_block_by_number(&url, block_num).await.unwrap();
							let header = block.clone().header;
							// let max_rows = header.extrinsics_root.rows;
							// let max_cols = header.extrinsics_root.cols;

							// let cells = get_kate_proof(&url, block_num, max_rows, max_cols, app_id as u32).await.unwrap();
							let mut vec = Vec::new();
							// for cell in cells.iter(){
							//     vec.push((cell.proof).clone());
							// }
							let cells =
								check_id(header, app_id as u32, block_num, block.clone()).unwrap();
							for i in cells.iter() {
								let data =
									get_kate_query_proof_by_cell(&url, block_num, i.row, i.col)
										.await
										.unwrap();
								vec.push(data);
							}
							mk_response(
								format!(r#"{{"block": {}, "data": {:?}}}"#, block_num, vec)
									.to_owned(),
							)
						} else {
							let mut not_found = Response::default();
							*not_found.status_mut() = StatusCode::NOT_FOUND;
							not_found
								.headers_mut()
								.insert(ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
							Ok(not_found)
						}
					} else {
						let mut not_found = Response::default();
						*not_found.status_mut() = StatusCode::NOT_FOUND;
						not_found
							.headers_mut()
							.insert(ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
						Ok(not_found)
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
	store: Arc<DBWithThreadMode<SingleThreaded>>,
	url: String,
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
		let url = self.url.clone();
		let fut = async move { Ok(Handler { store, url }) };
		Box::pin(fut)
	}
}

#[tokio::main]
pub async fn run_appdata_server(
	store: Arc<DBWithThreadMode<SingleThreaded>>,
	cfg: super::types::RuntimeConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
	let addr = format!("{}:{}", cfg.http_server_host, cfg.http_appdata_port)
		.parse()
		.expect("Bad Http server host/ port, found in config file");
	let rpc_url = check_http(cfg.full_node_rpc).await.unwrap();
	let server = Server::bind(&addr).serve(MakeHandler {
		store,
		url: rpc_url,
	});
	server.await?;
	Ok(())
}
