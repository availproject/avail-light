use super::{transactions, ws};
use crate::{
	api::{
		configuration::SharedConfig,
		types::{
			block_status, filter_fields, Block, BlockStatus, DataQuery, DataResponse,
			DataTransaction, Error, FieldsQueryParameter, Header, Status, SubmitResponse,
			Subscription, SubscriptionId, Transaction, Version, WsClients,
		},
	},
	data::{AppDataKey, BlockHeaderKey, Database, RpcNodeKey, VerifiedCellCountKey},
	utils::calculate_confidence,
};
use avail_rust::AvailHeader;
use color_eyre::{eyre::eyre, Result};
use std::{convert::Infallible, sync::Arc};
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
	submitter
		.submit(transaction)
		.await
		.map_err(Error::internal_server_error)
}

#[allow(clippy::too_many_arguments)]
pub async fn ws(
	subscription_id: String,
	ws: Ws,
	clients: WsClients,
	version: String,
	config: SharedConfig,
	submitter: Option<Arc<impl transactions::Submit + Clone + Send + Sync + 'static>>,
	db: impl Database + Clone + Send + 'static,
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
			submitter.clone(),
			db.clone(),
		)
	}))
}

pub fn version(version: String, db: impl Database) -> impl Reply {
	let network_version = db.get(RpcNodeKey).unwrap_or_default().system_version;
	Version {
		version,
		network_version,
	}
}

pub fn status(config: SharedConfig, db: impl Database) -> impl Reply {
	Status::new(&config, db)
}

pub async fn block(
	block_number: u32,
	config: SharedConfig,
	db: impl Database + Clone,
) -> Result<impl Reply, Error> {
	let sync_start_block = &config.sync_start_block;

	let block_status = db
		.get(BlockHeaderKey(block_number))
		.map(|AvailHeader { extension, .. }| extension)
		.and_then(|extension| block_status(sync_start_block, db.clone(), block_number, extension))
		.ok_or(Error::not_found())?;

	let confidence = db
		.get(VerifiedCellCountKey(block_number))
		.map(calculate_confidence);

	Ok(Block::new(block_status, confidence))
}

pub async fn block_header(
	block_number: u32,
	config: SharedConfig,
	db: impl Database + Clone,
) -> Result<Header, Error> {
	let sync_start_block = &config.sync_start_block;

	let block_status = db
		.get(BlockHeaderKey(block_number))
		.map(|AvailHeader { extension, .. }| extension)
		.and_then(|extension| block_status(sync_start_block, db.clone(), block_number, extension))
		.ok_or(Error::not_found())?;

	if matches!(
		block_status,
		BlockStatus::Unavailable | BlockStatus::Pending | BlockStatus::VerifyingHeader
	) {
		return Err(Error::bad_request_unknown("Block header is not available"));
	};

	db.get(BlockHeaderKey(block_number))
		.ok_or_else(|| eyre!("Header not found"))
		.and_then(|header| header.try_into())
		.map_err(Error::internal_server_error)
}

pub async fn block_data(
	block_number: u32,
	query: DataQuery,
	config: SharedConfig,
	db: impl Database + Clone,
) -> Result<DataResponse, Error> {
	let app_id = config.app_id.ok_or(Error::not_found())?;
	let sync_start_block = &config.sync_start_block;

	let block_status = db
		.get(BlockHeaderKey(block_number))
		.map(|AvailHeader { extension, .. }| extension)
		.and_then(|extension| block_status(sync_start_block, db.clone(), block_number, extension))
		.ok_or(Error::not_found())?;

	if block_status != BlockStatus::Finished && block_status != BlockStatus::Incomplete {
		return Err(Error::bad_request_unknown("Block data is not available"));
	};

	let data = db.get(AppDataKey(app_id, block_number));

	let Some(data) = data else {
		return Ok(DataResponse {
			block_number,
			data_transactions: vec![],
		});
	};

	let mut data_transactions: Vec<DataTransaction> = data
		.into_iter()
		.map(DataTransaction::try_from)
		.collect::<Result<_>>()
		.map_err(Error::internal_server_error)?;

	if let Some(FieldsQueryParameter(fields)) = &query.fields {
		filter_fields(&mut data_transactions, fields);
	}

	Ok(DataResponse {
		block_number,
		data_transactions,
	})
}
