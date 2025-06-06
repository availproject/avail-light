use async_trait::async_trait;
use avail_core::AppId;
use avail_rust::Keypair;
use color_eyre::Result;

use crate::{
	api::types::{SubmitResponse, Transaction},
	data::Database,
	network::rpc,
};

#[async_trait]
pub trait Submit {
	async fn submit(&self, transaction: Transaction) -> Result<SubmitResponse>;
}

#[derive(Clone)]
pub struct Submitter<T: Database> {
	pub rpc_client: rpc::Client<T>,
	pub app_id: u32,
	pub signer: Keypair,
}

#[async_trait]
impl<T: Database + Sync> Submit for Submitter<T> {
	async fn submit(&self, transaction: Transaction) -> Result<SubmitResponse> {
		let response = match transaction {
			Transaction::Data(data) => {
				self.rpc_client
					.submit_signed_and_wait_for_finalized(data, &self.signer, AppId(self.app_id))
					.await?
			},
			Transaction::Extrinsic(extrinsic) => {
				self.rpc_client
					.submit_from_bytes_and_wait_for_finalized(extrinsic.into())
					.await?
			},
		};

		let block_number = self
			.rpc_client
			.get_header_by_hash(response.block_hash)
			.await?
			.number;

		Ok(SubmitResponse {
			block_number,
			block_hash: response.block_hash,
			hash: response.hash,
			index: response.index,
		})
	}
}
