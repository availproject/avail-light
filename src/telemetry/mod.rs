use std::{net::SocketAddr, sync::Arc};

use anyhow::{Error, Ok};
use hyper::{
	server::conn::AddrStream,
	service::{make_service_fn, service_fn},
	Server,
};
use prometheus_client::registry::Registry;
use tokio::sync::Mutex;
use tracing::info;

pub mod handler;
pub mod metrics;

pub async fn http_server(prometheus_addr: SocketAddr, registry: Registry) -> Result<(), Error> {
	let listener = tokio::net::TcpListener::bind(&prometheus_addr)
		.await
		.map_err(|_| anyhow::anyhow!("Port already in use: {}", prometheus_addr))?;

	let context = Arc::new(Mutex::new(registry));

	let service = make_service_fn(move |_conn: &AddrStream| {
		let ctx = context.clone();
		let service = service_fn(move |req| handler::new(ctx.clone(), req));
		async move { Ok(service) }
	});

	let incoming = hyper::server::conn::AddrIncoming::from_listener(listener)?;
	let server = Server::builder(incoming).serve(service);
	info!("Metrics server on http://{}/metrics", server.local_addr());

	let result = server.await.map_err(Into::into);
	result
}
