//! HTTP server for confidence and data retrieval.
//!
//! # Endpoints
//!
//! * `/v1/mode` - returns client mode (light or light+app client)
//! * `/v1/status` - returns status of a latest processed block
//! * `/v1/latest_block` - returns latest processed block
//! * `/v1/confidence/{block_number}` - returns calculated confidence for a given block number
//! * `/v1/appdata/{block_number}` - returns decoded extrinsic data for configured app_id and given block number

use crate::api::v2;
use crate::types::IdentityConfig;
use crate::{
	api::v1,
	network::rpc::{self},
	types::{RuntimeConfig, State},
};
use color_eyre::eyre::WrapErr;
use rocksdb::DB;
use std::{
	net::SocketAddr,
	str::FromStr,
	sync::{Arc, Mutex},
};
use tracing::info;
use warp::{Filter, Reply};

pub struct Server {
	pub db: Arc<DB>,
	pub cfg: RuntimeConfig,
	pub identity_cfg: IdentityConfig,
	pub state: Arc<Mutex<State>>,
	pub version: String,
	pub network_version: String,
	pub node_client: rpc::Client,
	pub ws_clients: v2::types::WsClients,
}

fn health_route() -> impl Filter<Extract = impl Reply, Error = warp::Rejection> + Clone {
	warp::head()
		.or(warp::get())
		.and(warp::path("health"))
		.map(|_| warp::reply::with_status("", warp::http::StatusCode::OK))
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

		let v1_api = v1::routes(self.db.clone(), app_id, self.state.clone());
		let v2_api = v2::routes(
			self.version.clone(),
			self.network_version.clone(),
			self.state.clone(),
			self.cfg,
			self.identity_cfg,
			self.node_client.clone(),
			self.ws_clients.clone(),
			crate::data::RocksDB(self.db.clone()),
		);

		let cors = warp::cors()
			.allow_any_origin()
			.allow_header("content-type")
			.allow_methods(vec!["GET", "POST", "DELETE"]);

		let routes = health_route().or(v1_api).or(v2_api).with(cors);

		let addr = SocketAddr::from_str(format!("{host}:{port}").as_str())
			.wrap_err("Unable to parse host address from config")
			.unwrap();
		info!("RPC running on http://{host}:{port}");
		warp::serve(routes).run(addr).await;
	}
}
