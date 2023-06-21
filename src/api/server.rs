//! HTTP server for confidence and data retrieval.
//!
//! # Endpoints
//!
//! * `/v1/mode` - returns client mode (light or light+app client)
//! * `/v1/status` - returns status of a latest processed block
//! * `/v1/latest_block` - returns latest processed block
//! * `/v1/confidence/{block_number}` - returns calculated confidence for a given block number
//! * `/v1/appdata/{block_number}` - returns decoded extrinsic data for configured app_id and given block number

use crate::{
	api::v1::{self},
	types::RuntimeConfig,
};
use anyhow::Context;
use rand::{thread_rng, Rng};
use rocksdb::DB;
use std::{
	net::SocketAddr,
	str::FromStr,
	sync::{Arc, Mutex},
};
use tracing::info;
use warp::Filter;

/// Runs HTTP server
pub async fn run(store: Arc<DB>, cfg: RuntimeConfig, counter: Arc<Mutex<u32>>) {
	let host = cfg.http_server_host.clone();
	let port = if cfg.http_server_port.1 > 0 {
		let port: u16 = thread_rng().gen_range(cfg.http_server_port.0..=cfg.http_server_port.1);
		info!("Using random http server port: {port}");
		port
	} else {
		cfg.http_server_port.0
	};

	let v1_api = v1::routes(store.clone(), cfg.app_id, counter.clone());

	let cors = warp::cors()
		.allow_any_origin()
		.allow_header("content-type")
		.allow_methods(vec!["GET", "POST", "DELETE"]);

	let routes = v1_api.with(cors);

	let addr = SocketAddr::from_str(format!("{host}:{port}").as_str())
		.context("Unable to parse host address from config")
		.unwrap();
	info!("RPC running on http://{host}:{port}");
	warp::serve(routes).run(addr).await;
}
