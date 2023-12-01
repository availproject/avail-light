use super::{
	transactions,
	types::{Payload, Request, Response, Status, Version, WsClients, WsError, WsResponse},
};
use crate::{
	api::v2::types::{Error, Sender},
	types::{RuntimeConfig, State},
};
use color_eyre::{eyre::WrapErr, Result};
use futures::{FutureExt, StreamExt};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{error, log::warn};
use warp::ws::{self, Message, WebSocket};

#[allow(clippy::too_many_arguments)]
pub async fn connect(
	subscription_id: String,
	web_socket: WebSocket,
	clients: WsClients,
	version: Version,
	config: RuntimeConfig,
	submitter: Option<Arc<impl transactions::Submit + Clone + Send + Sync + 'static>>,
	state: Arc<Mutex<State>>,
) {
	let (web_socket_sender, mut web_socket_receiver) = web_socket.split();
	let (sender, receiver) = mpsc::unbounded_channel();
	let receiver_stream = UnboundedReceiverStream::new(receiver);

	if let Err(error) = clients.set_sender(&subscription_id, sender.clone()).await {
		error!("Cannot set sender: {error}");
		return;
	};

	tokio::task::spawn(receiver_stream.forward(web_socket_sender).map(|result| {
		if let Err(error) = result {
			error!("Error sending web socket message: {error}");
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
				error!("Error receiving client message: {error}");
				continue;
			},
			Ok(message) if !message.is_text() => continue,
			Ok(message) => message,
		};

		let submitter = submitter.clone();
		let state = state.clone();

		let send_result = match handle_request(message, &version, &config, submitter, state).await {
			Ok(response) => send(sender.clone(), response),
			Err(error) => {
				if let Some(cause) = error.cause.as_ref() {
					error!("Failed to handle request: {cause:#}");
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
	version: &Version,
	config: &RuntimeConfig,
	submitter: Option<Arc<impl transactions::Submit>>,
	state: Arc<Mutex<State>>,
) -> Result<WsResponse, Error> {
	let request = Request::try_from(message).map_err(|error| {
		Error::bad_request_unknown(&format!("Failed to parse request: {error}"))
	})?;

	let request_id = request.request_id;
	match request.payload {
		Payload::Version => Ok(Response::new(request_id, version.clone()).into()),
		Payload::Status => {
			let state = state.lock().expect("State lock can be acquired");
			let status = Status::new(config, &state);
			Ok(Response::new(request_id, status).into())
		},
		Payload::Submit(transaction) => {
			let Some(submitter) = submitter else {
				return Err(Error::bad_request(request_id, "Submit is not configured."));
			};
			if transaction.is_empty() {
				return Err(Error::bad_request(request_id, "Transaction is empty."));
			}

			submitter
				.submit(transaction)
				.await
				.map(|response| Response::new(request_id, response).into())
				.map_err(Error::internal_server_error)
		},
	}
}
