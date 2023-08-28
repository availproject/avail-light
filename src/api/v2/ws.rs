use super::types::{Clients, Request, RequestType, Response, ResponseTopic, Status, Version};
use crate::{
	api::v2::types::Error,
	rpc::Node,
	types::{RuntimeConfig, State},
};
use anyhow::{anyhow, Context};
use futures::{FutureExt, StreamExt};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{error, info, warn};
use warp::ws::{self, Message, WebSocket};

pub async fn connect(
	subscription_id: String,
	web_socket: WebSocket,
	clients: Clients,
	version: Version,
	config: RuntimeConfig,
	node: Node,
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
		match result {
			Ok(message) if !message.is_text() => continue,
			Ok(message) => {
				if let Some(error) = match <Request>::try_from(message) {
					Ok(request) => {
						if let Err(error) = handle_websocket_message(
							request,
							sender.clone(),
							&version,
							&config,
							&node,
							state.clone(),
						) {
							error!("Error handling web socket message: {error}");
							Some(Error::internal_server_error())
						} else {
							None
						}
					},
					Err(error) => {
						warn!("Error handling web socket message: {error}");
						Some(Error::bad_request(
							"Error handling web socket message: Failed to parse request"
								.to_string(),
						))
					},
				} {
					if let Err(error) = sender.send(Ok(Message::text::<String>(error.into()))) {
						error!("{error}");
					};
				};
			},
			Err(error) => {
				error!("Error receiving client message: {error}");
			},
		}
	}
}

fn send_websocket_message<T: Serialize>(
	sender: UnboundedSender<Result<Message, warp::Error>>,
	response: Response<T>,
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

fn handle_websocket_message(
	request: Request,
	sender: UnboundedSender<Result<Message, warp::Error>>,
	version: &Version,
	config: &RuntimeConfig,
	node: &Node,
	state: Arc<Mutex<State>>,
) -> anyhow::Result<()> {
	match request.request_type {
		RequestType::Version => send_websocket_message(
			sender,
			Response {
				topic: ResponseTopic::Version,
				request_id: request.request_id,
				message: version.clone(),
			},
		)?,
		RequestType::Status => {
			let state = state
				.lock()
				.map_err(|error| anyhow!("Cannot acquire state lock: {error}"))?;

			let status = Status::new(config, node, &state);

			send_websocket_message(
				sender,
				Response {
					topic: ResponseTopic::Status,
					request_id: request.request_id,
					message: status,
				},
			)?;
		},
	};

	Ok(())
}
