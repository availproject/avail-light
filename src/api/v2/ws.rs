use crate::api::v2::types::{Error, ErrorCode};

use super::types::{Clients, Request, RequestType, Response, ResponseTopic, Version};
use anyhow::Context;
use futures::{FutureExt, StreamExt};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{error, info};
use warp::ws::{self, Message, WebSocket};

pub async fn connect(
	subscription_id: String,
	web_socket: WebSocket,
	clients: Clients,
	version: Version,
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
			Ok(message) if !message.is_text() => return,
			Ok(message) => {
				if let Err(error) = handle_websocket_message(message, sender.clone(), &version) {
					let error = Error {
						error_code: ErrorCode::BadRequest,
						message: format!("Error handling web socket message: {error}"),
					};
					error!(error.message);
					if let Err(error) = sender.send(Ok(Message::text::<String>(error.into()))) {
						error!("{error}");
					};
					break;
				};
			},
			Err(error) => {
				error!("Error receiving client message: {error}");
				break;
			},
		}
	}
}

fn handle_websocket_message(
	message: ws::Message,
	sender: UnboundedSender<Result<Message, warp::Error>>,
	version: &Version,
) -> anyhow::Result<()> {
	let request: Request = message.try_into().context("Failed to parse request")?;

	match request.request_type {
		RequestType::Version => {
			let response = Response {
				topic: ResponseTopic::Version,
				request_id: request.request_id,
				message: version.clone(),
			};

			let message = serde_json::to_string(&response)
				.map(ws::Message::text)
				.map(Ok)
				.context("Failed to serialize version")?;

			sender.send(message).context("Failed to send message")?;
		},
	}
	Ok(())
}
