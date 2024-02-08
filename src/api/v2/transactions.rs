use async_trait::async_trait;
use avail_subxt::{api, primitives::AvailExtrinsicParams, AvailConfig};
use color_eyre::Result;
use sp_core::sr25519::Pair;
use subxt::tx::PairSigner;

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
	pub pair_signer: PairSigner<AvailConfig, Pair>,
}

#[async_trait]
impl Submit for Submitter {
	async fn submit(&self, transaction: Transaction) -> Result<SubmitResponse> {
		let ex_event = match transaction {
			Transaction::Data(data) => {
				let extrinsic = api::tx().data_availability().submit_data(data.into());
				let params = AvailExtrinsicParams::new_with_app_id(self.app_id.into());
				self.rpc_client
					.submit_signed_and_wait_for_finalized(&extrinsic, &self.pair_signer, params)
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
