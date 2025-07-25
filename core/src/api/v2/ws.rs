use super::transactions;
use crate::{
	api::{
		configuration::SharedConfig,
		types::{Error, Request, Sender, WsClients, WsError, WsResponse},
		v2::messages,
	},
	data::Database,
	utils::spawn_in_span,
};
use color_eyre::{eyre::WrapErr, Result};
use futures::{FutureExt, StreamExt};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{error, log::warn};
use warp::ws::{self, Message, WebSocket};

#[allow(clippy::too_many_arguments)]
pub async fn connect(
	subscription_id: String,
	web_socket: WebSocket,
	clients: WsClients,
	version: String,
	config: SharedConfig,
	submitter: Option<Arc<impl transactions::Submit + Clone + Send + Sync + 'static>>,
	db: impl Database + Clone,
) {
	let (web_socket_sender, mut web_socket_receiver) = web_socket.split();
	let (sender, receiver) = mpsc::unbounded_channel();
	let receiver_stream = UnboundedReceiverStream::new(receiver);

	if let Err(error) = clients.set_sender(&subscription_id, sender.clone()).await {
		error!(%error, event_type = "WS_CONNECT", api_version = "v2","Cannot set sender");
		return;
	};

	spawn_in_span(receiver_stream.forward(web_socket_sender).map(|result| {
		if let Err(error) = result {
		    error!(%error, event_type = "WS_CONNECT", api_version = "v2", "Error sending web socket message");
		}
	}));

	fn send<T: Serialize>(sender: Sender, message: T) -> Result<()> {
		let ws_message = serde_json::to_string(&message)
			.map(ws::Message::text)
			.wrap_err("Failed to serialize message")?;

		sender
			.send(Ok(ws_message))
			.wrap_err("Failed to send message")
	}

	while let Some(result) = web_socket_receiver.next().await {
		let message = match result {
			Err(error) => {
				error!(%error, event_type = "WS_CONNECT", api_version = "v2", "Error receiving client message");
				continue;
			},
			Ok(message) if !message.is_text() => continue,
			Ok(message) => message,
		};

		let submitter = submitter.clone();

		let send_result =
			match handle_request(message, &version, &config, submitter, db.clone()).await {
				Ok(response) => send(sender.clone(), response),
				Err(error) => {
					if let Some(cause) = error.cause.as_ref() {
						error!(%cause, event_type = "WS_CONNECT", api_version = "v2", "Failed to handle request");
					};
					send::<WsError>(sender.clone(), error.into())
				},
			};

		if let Err(error) = send_result {
			warn!("Error sending message: {error:#}");
		}
	}
}

async fn handle_request(
	message: Message,
	version: &str,
	config: &SharedConfig,
	submitter: Option<Arc<impl transactions::Submit>>,
	db: impl Database,
) -> Result<WsResponse, Error> {
	let request = Request::try_from(message).map_err(|error| {
		Error::bad_request_unknown(&format!("Failed to parse request: {error}"))
	})?;

	messages::handle_request(request, version, config, submitter, db).await
}
