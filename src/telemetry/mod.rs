use std::net::SocketAddr;

use anyhow::Error;
use hyper::Server;
use prometheus_client::registry::Registry;
use tracing::info;

use http_service::MakeMetricService;

pub mod http_service;
pub mod metrics;

pub async fn http_server(prometheus_addr: SocketAddr, registry: Registry) -> Result<(), Error> {
	let listener = tokio::net::TcpListener::bind(&prometheus_addr)
		.await
		.map_err(|_| anyhow::anyhow!("Port already in use: {}", prometheus_addr))?;

	let listener = hyper::server::conn::AddrIncoming::from_listener(listener)?;

	let server = Server::builder(listener).serve(MakeMetricService::new(registry));
	info!("Metrics server on http://{}/metrics", server.local_addr());

	let result = server.await.map_err(Into::into);
	result
}
