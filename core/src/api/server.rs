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
use prometheus_client::{encoding::text::encode, registry::Registry};
use std::{
	net::SocketAddr,
	str::FromStr,
	sync::{Arc, Mutex},
};
use tracing::error;
use tracing::info;
use warp::{http, reply, Filter, Rejection, Reply};

use super::configuration::{APIConfig, SharedConfig};
use super::types::WsClients;

pub struct Server<T: Database> {
	pub db: T,
	pub cfg: SharedConfig,
	pub identity_cfg: IdentityConfig,
	pub version: String,
	pub node_client: rpc::Client<T>,
	pub ws_clients: WsClients,
	pub shutdown: Controller<String>,
	pub p2p_client: p2p::Client,
	pub metric_registry: Option<Arc<Mutex<Registry>>>,
}

async fn metrics_handler(registry: Option<Arc<Mutex<Registry>>>) -> Result<impl Reply, Rejection> {
	let Some(registry) = registry else {
		return Ok(
			reply::with_status("Metrics not found", http::StatusCode::NOT_FOUND).into_response(),
		);
	};

	let registry = registry.lock().unwrap();
	let mut buffer = String::new();
	encode(&mut buffer, &registry).unwrap();

	Ok(reply::with_header(
		reply::with_status(buffer, http::StatusCode::OK),
		"Content-Type",
		"application/openmetrics-text;charset=utf-8;version=1.0.0",
	)
	.into_response())
}

fn metrics_route(
	registry: Option<Arc<Mutex<Registry>>>,
) -> impl Filter<Extract = impl Reply, Error = warp::Rejection> + Clone {
	warp::get()
		.and(warp::path("metrics"))
		.and_then(move || metrics_handler(registry.clone()))
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
		let diagnostics_api = diagnostics::routes(self.p2p_client);

		let cors = warp::cors()
			.allow_any_origin()
			.allow_header("content-type")
			.allow_methods(vec!["GET", "POST", "DELETE"]);

		let routes = health_route()
			.or(metrics_route(self.metric_registry.clone()))
			.or(v1_api)
			.or(v2_api)
			.or(diagnostics_api)
			.with(cors);

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
		return Ok(http::StatusCode::INTERNAL_SERVER_ERROR.into_response());
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
