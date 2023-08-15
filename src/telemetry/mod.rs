use anyhow::{Error, Ok};
use hyper::{
	server::conn::AddrStream,
	service::{make_service_fn, service_fn},
	Server,
};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::Mutex;

use prometheus_client::registry::Registry;
use tracing::info;

pub mod handler;
pub mod metrics;
pub mod otlp;

pub async fn bind(prometheus_addr: SocketAddr) -> Result<hyper::server::conn::AddrIncoming, Error> {
	let listener = tokio::net::TcpListener::bind(&prometheus_addr)
		.await
		.map_err(|_| anyhow::anyhow!("Port already in use: {}", prometheus_addr))?;
	hyper::server::conn::AddrIncoming::from_listener(listener).map_err(Into::into)
}

pub async fn http_server(
	incoming: hyper::server::conn::AddrIncoming,
	registry: Registry,
) -> Result<(), Error> {
	let context = Arc::new(Mutex::new(registry));

	let service = make_service_fn(move |_conn: &AddrStream| {
		let ctx = context.clone();
		let service = service_fn(move |req| handler::new(ctx.clone(), req));
		async move { Ok(service) }
	});

	let server = Server::builder(incoming).serve(service);
	info!("Metrics server on http://{}/metrics", server.local_addr());

	server.await.map_err(Into::into)
}
