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
	api::{
		diagnostics,
		types::{Error, ErrorCode, InternalServerError},
		v1, v2,
	},
	data::Database,
	network::{p2p, rpc},
	shutdown::Controller,
	types::IdentityConfig,
};

use color_eyre::eyre::WrapErr;
use futures::{Future, FutureExt};
use hyper::StatusCode;
use std::{net::SocketAddr, str::FromStr};
use tracing::error;
use tracing::info;
use warp::{Filter, Rejection, Reply};

use super::{
	configuration::{APIConfig, SharedConfig},
	types::WsClients,
};

pub struct Server<T: Database> {
	pub db: T,
	pub cfg: SharedConfig,
	pub identity_cfg: IdentityConfig,
	pub version: String,
	pub node_client: rpc::Client<T>,
	pub ws_clients: WsClients,
	pub shutdown: Controller<String>,
	pub p2p_client: Option<p2p::Client>,
}

fn health_route() -> impl Filter<Extract = impl Reply, Error = warp::Rejection> + Clone {
	warp::head()
		.or(warp::get())
		.and(warp::path("health"))
		.map(|_| warp::reply::with_status("", warp::http::StatusCode::OK))
}

impl<T: Database + Clone + Send + Sync + 'static> Server<T> {
	/// Creates a HTTP server that needs to be spawned into a runtime
	pub fn bind(self, cfg: APIConfig) -> impl Future<Output = ()> {
		let host = cfg.http_server_host.clone();
		let port = cfg.http_server_port;
		let v1_api = v1::routes(self.db.clone(), self.cfg.clone());
		let v2_api = v2::routes(
			self.version.clone(),
			self.cfg,
			self.identity_cfg,
			self.node_client.clone(),
			self.ws_clients.clone(),
			self.db.clone(),
		);
		let cors = warp::cors()
			.allow_any_origin()
			.allow_header("content-type")
			.allow_methods(vec!["GET", "POST", "DELETE"]);

		let base_routes = health_route().or(v1_api).or(v2_api);

		// Add P2P routes depending on whether P2P is enabled
		let p2p_routes = if let Some(p2p_client) = &self.p2p_client {
			diagnostics::routes(p2p_client.clone())
		} else {
			diagnostics::p2p_disabled_routes()
		};

		// Combine all routes
		let routes = base_routes.or(p2p_routes).with(cors);

		let addr = SocketAddr::from_str(format!("{host}:{port}").as_str())
			.wrap_err("Unable to parse host address from config")
			.unwrap();
		info!("RPC running on http://{host}:{port}");
		// warp graceful shutdown expects a signal that is [`Future<Output = ()>`]
		let shutdown_signal = self.shutdown.triggered_shutdown().map(|_| ());
		let (_, server) = warp::serve(routes).bind_with_graceful_shutdown(addr, shutdown_signal);

		server
	}
}

pub async fn handle_rejection(error: Rejection) -> Result<impl Reply, Rejection> {
	if error.find::<InternalServerError>().is_some() {
		return Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response());
	}
	Err(error)
}

pub fn log_internal_server_error(result: Result<impl Reply, Error>) -> Result<impl Reply, Error> {
	if let Err(Error {
		error_code: ErrorCode::InternalServerError,
		cause: Some(error),
		message,
		..
	}) = result.as_ref()
	{
		error!("{message}: {error:#}");
	}
	result
}
