use async_trait::async_trait;
use avail_core::AppId;
use color_eyre::Result;
use subxt_signer::sr25519::Keypair;

use super::types::{SubmitResponse, Transaction};
use crate::network::rpc;

#[async_trait]
pub trait Submit {
	async fn submit(&self, transaction: Transaction) -> Result<SubmitResponse>;
}

#[derive(Clone)]
pub struct Submitter {
	pub rpc_client: rpc::Client,
	pub app_id: u32,
	pub pair_signer: Keypair,
}

#[async_trait]
impl Submit for Submitter {
	async fn submit(&self, transaction: Transaction) -> Result<SubmitResponse> {
		let ex_event = match transaction {
			Transaction::Data(data) => {
				self.rpc_client
					.submit_signed_and_wait_for_finalized(
						data.into(),
						&self.pair_signer,
						AppId(self.app_id),
					)
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
			.get_header_by_hash(ex_event.block_hash())
			.await?
			.number;

		Ok(SubmitResponse {
			block_number,
			block_hash: ex_event.block_hash(),
			hash: ex_event.extrinsic_hash(),
			index: ex_event.extrinsic_index(),
		})
	}
}
