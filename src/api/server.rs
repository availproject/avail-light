//! HTTP server for confidence and data retrieval.
//!
//! # Endpoints
//!
//! * `/v1/mode` - returns client mode (light or light+app client)
//! * `/v1/status` - returns status of a latest processed block
//! * `/v1/latest_block` - returns latest processed block
//! * `/v1/confidence/{block_number}` - returns calculated confidence for a given block number
//! * `/v1/appdata/{block_number}` - returns decoded extrinsic data for configured app_id and given block number

#[cfg(feature = "api-v2")]
use crate::api::v2;
use crate::{
	api::v1,
	rpc::Node,
	types::{RuntimeConfig, State},
};
use anyhow::Context;
use avail_subxt::avail;
use rand::{thread_rng, Rng};
use rocksdb::DB;
use std::{
	net::SocketAddr,
	str::FromStr,
	sync::{Arc, Mutex},
};
use tracing::info;
use warp::Filter;

pub struct Server {
	pub db: Arc<DB>,
	pub cfg: RuntimeConfig,
	pub state: Arc<Mutex<State>>,
	pub version: String,
	pub network_version: String,
	pub node: Node,
	pub node_client: avail::Client,
}

impl Server {
	/// Runs HTTP server
	pub async fn run(self) {
		let RuntimeConfig {
			http_server_host: host,
			http_server_port: port,
			app_id,
			..
		} = self.cfg.clone();

		let port = (port.1 > 0)
			.then(|| thread_rng().gen_range(port.0..=port.1))
			.unwrap_or(port.0);

		let v1_api = v1::routes(self.db.clone(), app_id, self.state.clone());
		#[cfg(feature = "api-v2")]
		let v2_api = v2::routes(
			self.version.clone(),
			self.network_version.clone(),
			self.node,
			self.state.clone(),
			self.cfg,
			self.node_client.clone(),
		);

		let cors = warp::cors()
			.allow_any_origin()
			.allow_header("content-type")
			.allow_methods(vec!["GET", "POST", "DELETE"]);

		let routes = v1_api;
		#[cfg(feature = "api-v2")]
		let routes = routes.or(v2_api);
		let routes = routes.with(cors);

		let addr = SocketAddr::from_str(format!("{host}:{port}").as_str())
			.context("Unable to parse host address from config")
			.unwrap();
		info!("RPC running on http://{host}:{port}");
		warp::serve(routes).run(addr).await;
	}
}
