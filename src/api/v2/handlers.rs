use super::{
	transactions,
	types::{
		block_status, Block, Error, Status, SubmitResponse, Subscription, SubscriptionId,
		Transaction, Version, WsClients,
	},
	ws,
};
use crate::{
	api::v2::types::InternalServerError,
	data::Database,
	rpc::Node,
	types::{RuntimeConfig, State},
	utils::calculate_confidence,
};
use hyper::StatusCode;
use std::{
	convert::Infallible,
	sync::{Arc, Mutex},
};
use tracing::error;
use uuid::Uuid;
use warp::{ws::Ws, Rejection, Reply};

pub async fn subscriptions(
	subscription: Subscription,
	clients: WsClients,
) -> Result<SubscriptionId, Infallible> {
	let subscription_id = Uuid::new_v4().to_string();
	clients.subscribe(&subscription_id, subscription).await;
	Ok(SubscriptionId { subscription_id })
}

pub async fn submit(
	submitter: Arc<impl transactions::Submit>,
	transaction: Transaction,
) -> Result<SubmitResponse, Error> {
	if matches!(&transaction, Transaction::Data(_)) && !submitter.has_signer() {
		return Err(Error::not_found());
	};

	submitter.submit(transaction).await.map_err(|error| {
		error!(%error, "Submit transaction failed");
		Error::internal_server_error(error)
	})
}

#[allow(clippy::too_many_arguments)]
pub async fn ws(
	subscription_id: String,
	ws: Ws,
	clients: WsClients,
	version: Version,
	config: RuntimeConfig,
	node: Node,
	submitter: Option<Arc<impl transactions::Submit + Clone + Send + Sync + 'static>>,
	state: Arc<Mutex<State>>,
) -> Result<impl Reply, Rejection> {
	if !clients.has_subscription(&subscription_id).await {
		return Err(warp::reject::not_found());
	}
	// NOTE: Multiple connections to the same client are currently allowed
	Ok(ws.on_upgrade(move |web_socket| {
		ws::connect(
			subscription_id,
			web_socket,
			clients,
			version,
			config,
			node,
			submitter.clone(),
			state.clone(),
		)
	}))
}

pub fn status(config: RuntimeConfig, node: Node, state: Arc<Mutex<State>>) -> impl Reply {
	let state = state.lock().expect("Lock should be acquired");
	Status::new(&config, &node, &state)
}

pub async fn block(
	block_number: u32,
	config: RuntimeConfig,
	state: Arc<Mutex<State>>,
	db: impl Database,
) -> Result<impl Reply, Error> {
	let state = state.lock().expect("Lock should be acquired");

	let Some(block_status) = block_status(&config, &state, block_number) else {
		return Err(Error::not_found());
	};

	let confidence = db
		.get_confidence(block_number)
		.map_err(Error::internal_server_error)?
		.map(calculate_confidence);

	Ok(Block::new(block_status, confidence))
}

pub async fn handle_rejection(error: Rejection) -> Result<impl Reply, Rejection> {
	if error.find::<InternalServerError>().is_some() {
		return Ok(StatusCode::INTERNAL_SERVER_ERROR.into_response());
	}
	Err(error)
}
