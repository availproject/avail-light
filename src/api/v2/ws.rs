use super::{
	transactions,
	types::{Clients, Payload, Request, Response, Status, Transaction, Version, WsResponse},
};
use crate::{
	api::v2::types::Error,
	rpc::Node,
	types::{RuntimeConfig, State},
};
use anyhow::Context;
use futures::{FutureExt, StreamExt};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{error, info, warn};
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

	while let Some(result) = web_socket_receiver.next().await {
		let message = match result {
			Err(error) => {
				error!("Error receiving client message: {error}");
				continue;
			},
			Ok(message) if !message.is_text() => continue,
			Ok(message) => message,
		};

		let request = match Request::try_from(message) {
			Err(error) => {
				warn!("Error handling web socket message: {error}");
				let error =
					Error::bad_request_unknown(&format!("Failed to parse request: {error}"));
				if let Err(error) = send_response(sender.clone(), error.into()) {
					error!("Failed to send error: {error}");
				}
				continue;
			},
			Ok(request) => request,
		};

		let submitter = submitter.clone();
		let state = state.clone();

		let response = handle_request(request, &version, &config, &node, submitter, state)
			.await
			.unwrap_or_else(|error| {
				error!("Error handling web socket message: {error}");
				Error::internal_server_error().into()
			});

		if let Err(error) = send_response(sender.clone(), response) {
			error!("Failed to send response: {error}");
		}
	}
}

fn send_response(
	sender: UnboundedSender<Result<Message, warp::Error>>,
	response: WsResponse,
) -> anyhow::Result<()> {
	let message = serde_json::to_string(&response)
		.map(ws::Message::text)
		.map(Ok)
		.context("Failed to serialize response message")?;

	sender
		.send(message)
		.context("Failed to send response message")?;
	Ok(())
}

async fn handle_request(
	request: Request,
	version: &Version,
	config: &RuntimeConfig,
	node: &Node,
	submitter: Option<Arc<impl transactions::Submit>>,
	state: Arc<Mutex<State>>,
) -> anyhow::Result<WsResponse> {
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
				return Ok(Error::bad_request(request_id, "Submit is not configured.").into());
			};
			if transaction.is_empty() {
				return Ok(Error::bad_request(request_id, "Transaction is empty.").into());
			}
			if matches!(transaction, Transaction::Data(_)) && !submitter.has_signer() {
				return Ok(Error::bad_request(request_id, "Signer is not configured.").into());
			};

			let submit_response = submitter.submit(transaction).await?;
			Ok(Response::new(request_id, submit_response).into())
		},
	}
}
