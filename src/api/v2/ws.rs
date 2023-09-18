use super::{
	transactions,
	types::{
		Clients, Payload, Request, Response, Status, Transaction, Version, WsError, WsResponse,
	},
};
use crate::{
	api::v2::types::{Error, Sender},
	rpc::Node,
	types::{RuntimeConfig, State},
};
use anyhow::Context;
use futures::{FutureExt, StreamExt};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{error, info};
use warp::ws::{self, Message, WebSocket};

#[allow(clippy::too_many_arguments)]
pub async fn connect(
	subscription_id: String,
	web_socket: WebSocket,
	clients: Clients,
	version: Version,
	config: RuntimeConfig,
	node: Node,
	submitter: Option<Arc<impl transactions::Submit + Clone + Send + Sync + 'static>>,
	state: Arc<Mutex<State>>,
) {
	let (web_socket_sender, mut web_socket_receiver) = web_socket.split();
	let (sender, receiver) = mpsc::unbounded_channel();
	let receiver_stream = UnboundedReceiverStream::new(receiver);

	let mut clients = clients.write().await;
	let Some(client) = clients.get_mut(&subscription_id) else {
		info!("Client is not subscribed");
		return;
	};
	client.sender = Some(sender.clone());

	tokio::task::spawn(receiver_stream.forward(web_socket_sender).map(|result| {
		if let Err(error) = result {
			error!("Error sending web socket message: {error}");
		}
	}));

	fn send<T: Serialize>(sender: Sender, message: T) -> anyhow::Result<()> {
		let ws_message = serde_json::to_string(&message)
			.map(ws::Message::text)
			.context("Failed to serialize message")?;

		sender
			.send(Ok(ws_message))
			.context("Failed to send message")
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

		let send_result =
			match handle_request(message, &version, &config, &node, submitter, state).await {
				Ok(response) => send(sender.clone(), response),
				Err(error) => {
					if let Some(cause) = error.cause.as_ref() {
						error!("Failed to handle request: {cause}");
					};
					send::<WsError>(sender.clone(), error.into())
				},
			};

		if let Err(error) = send_result {
			error!("Error sending message: {error}");
		}
	}
}

async fn handle_request(
	message: Message,
	version: &Version,
	config: &RuntimeConfig,
	node: &Node,
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
			let status = Status::new(config, node, &state);
			Ok(Response::new(request_id, status).into())
		},
		Payload::Submit(transaction) => {
			let Some(submitter) = submitter else {
				return Err(Error::bad_request(request_id, "Submit is not configured."));
			};
			if transaction.is_empty() {
				return Err(Error::bad_request(request_id, "Transaction is empty."));
			}
			if matches!(transaction, Transaction::Data(_)) && !submitter.has_signer() {
				return Err(Error::bad_request(request_id, "Signer is not configured."));
			};

			submitter
				.submit(transaction)
				.await
				.map(|response| Response::new(request_id, response).into())
				.map_err(Error::internal_server_error)
		},
	}
}
