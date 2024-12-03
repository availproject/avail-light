use super::transactions;
use crate::{
	api::{
		configuration::SharedConfig,
		types::{Error, Payload, Request, Response, Status, Version, WsResponse},
	},
	data::{Database, RpcNodeKey},
};
use std::sync::Arc;

pub async fn handle_request(
	request: Request,
	version: &str,
	config: &SharedConfig,
	submitter: Option<Arc<impl transactions::Submit>>,
	db: impl Database,
) -> Result<WsResponse, Error> {
	let request_id = request.request_id;
	match request.payload {
		Payload::Version => {
			let version = Version {
				version: version.to_string(),
				network_version: db.get(RpcNodeKey).unwrap_or_default().system_version,
			};
			Ok(Response::new(request_id, version).into())
		},
		Payload::Status => {
			let status = Status::new(config, db);
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
