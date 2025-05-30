#[cfg(not(target_arch = "wasm32"))]
use super::configuration::SharedConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::{
	api::types::{PublishMessage, Topic},
	data::Database,
	network::rpc::Client,
};
#[cfg(not(target_arch = "wasm32"))]
use std::{fmt::Display, sync::Arc};
#[cfg(not(target_arch = "wasm32"))]
use tokio::sync::broadcast;
#[cfg(not(target_arch = "wasm32"))]
use tracing::{debug, error, info};
#[cfg(not(target_arch = "wasm32"))]
use warp::{Filter, Rejection, Reply};

#[cfg(not(target_arch = "wasm32"))]
use crate::{
	api::{server::handle_rejection, types::WsClients},
	types::IdentityConfig,
};

#[cfg(not(target_arch = "wasm32"))]
mod handlers;
pub mod messages;
#[cfg(not(target_arch = "wasm32"))]
mod routes;
pub mod transactions;
#[cfg(not(target_arch = "wasm32"))]
mod ws;

#[cfg(not(target_arch = "wasm32"))]
pub async fn publish<T: Clone + TryInto<Option<PublishMessage>>>(
	topic: Topic,
	mut receiver: broadcast::Receiver<T>,
	clients: WsClients,
) where
	<T as TryInto<Option<PublishMessage>>>::Error: Display,
{
	loop {
		let Ok(message) = receiver.recv().await else {
			info!(?topic, "Receiver is closed, stopping publisher...");
			return;
		};

		let message: Option<PublishMessage> = match message.try_into() {
			Ok(Some(message)) => Some(message),
			Ok(None) => continue, // Silently skip
			Err(error) => {
				error!(?topic, "Cannot create message: {error}");
				continue;
			},
		};

		if let Some(message) = message {
			match clients.publish(&topic, message).await {
				Ok(results) => {
					let published = results.iter().filter(|&result| result.is_ok()).count();
					let failed = results.iter().filter(|&result| result.is_err()).count();
					info!(?topic, published, failed, "Message published to clients");
					for error in results.into_iter().filter_map(Result::err) {
						debug!(?topic, "Cannot publish message to client: {error}")
					}
				},
				Err(error) => error!(?topic, "Cannot publish message: {error}"),
			}
		}
	}
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub fn routes(
	version: String,
	config: SharedConfig,
	identity_config: IdentityConfig,
	rpc_client: Client<impl Database + Send + Sync + Clone + 'static>,
	ws_clients: WsClients,
	db: impl Database + Clone + Send + 'static,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	let app_id = config.app_id.as_ref();

	let submitter = app_id.map(|&app_id| {
		Arc::new(transactions::Submitter {
			rpc_client,
			app_id,
			signer: identity_config.avail_key_pair,
		})
	});

	use routes::*;

	version_route(version.clone(), db.clone())
		.or(status_route(config.clone(), db.clone()))
		.or(block_route(config.clone(), db.clone()))
		.or(block_header_route(config.clone(), db.clone()))
		.or(block_data_route(config.clone(), db.clone()))
		.or(subscriptions_route(ws_clients.clone()))
		.or(submit_route(submitter.clone()))
		.or(ws_route(ws_clients, version, config, submitter, db.clone()))
		.recover(handle_rejection)
}
