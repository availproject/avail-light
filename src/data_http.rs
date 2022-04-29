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
use rocksdb::{DBWithThreadMode, SingleThreaded};
use tokio;

use crate::rpc::{
	check_http, check_id, get_block_by_number, get_headers, get_kate_query_proof_by_cell,
	match_block_url, match_id_url,
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

		let db = self.store.clone();
		let url = self.url.clone();

		Box::pin(async move {
			let res = match req.method() {
				&Method::GET => {
					if let (Ok(block_num), Ok(app_id)) = (
						match_block_url(req.uri().path()),
						match_id_url(req.uri().path()),
					) {
						let block = get_block_by_number(&url, block_num).await.unwrap();
						let headers = get_headers(
							db.clone(),
							db.cf_handle(crate::consts::BLOCK_HEADER_CF).unwrap(),
							block_num,
						)
						.unwrap();
						let mut vec = Vec::new();
						//cell logic to be written here
						// @TODO: fetching logic to be re written with optimisation
						match app_id {
							-1 => {
								let max_cols = headers.extrinsics_root.cols;
								let max_rows = headers.extrinsics_root.rows;
								for i in 0..max_rows {
									for j in 0..max_cols {
										let cells =
											get_kate_query_proof_by_cell(&url, block_num, i, j)
												.await
												.unwrap();
										vec.push(cells);
									}
								}
							},
							0 => {
								vec.push(vec![]);
							},
							_ => {
								let cells =
									check_id(headers, app_id as u32, block_num, block.clone())
										.unwrap();
								for i in cells.iter() {
									let data =
										get_kate_query_proof_by_cell(&url, block_num, i.row, i.col)
											.await
											.unwrap();
									vec.push(data);
								}
							},
						}
						mk_response(
							format!(r#"{{"block": {}, "data": {:?}}}"#, block_num, vec).to_owned(),
						)
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
//service handler for the data server
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
	// @TODO: need to add routing
	let addr = format!("{}:{}", cfg.http_server_host, cfg.http_data_port)
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
