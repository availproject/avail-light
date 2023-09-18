use crate::rpc::{AvailSigner, RpcClient};

use super::types::{SubmitResponse, Transaction};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use avail_subxt::{api, primitives::AvailExtrinsicParams};

#[async_trait]
pub trait Submit {
	async fn submit(&self, transaction: Transaction) -> Result<SubmitResponse>;
	fn has_signer(&self) -> bool;
}

#[derive(Clone)]
pub struct Submitter {
	pub rpc: RpcClient,
	pub app_id: u32,
	pub pair_signer: Option<AvailSigner>,
}

#[async_trait]
impl Submit for Submitter {
	async fn submit(&self, transaction: Transaction) -> Result<SubmitResponse> {
		match transaction {
			Transaction::Data(data) => {
				let Some(pair_signer) = self.pair_signer.as_ref() else {
					return Err(anyhow!("Pair signer is not configured"));
				};
				let extrinsic = api::tx().data_availability().submit_data(data.into());
				let params = AvailExtrinsicParams::new_with_app_id(self.app_id.into());
				self.rpc
					.ext_sign_and_submit_then_wait_for_finalized_success(
						&extrinsic,
						pair_signer,
						params,
					)
					.await
			},
			Transaction::Extrinsic(extrinsic) => {
				self.rpc
					.ext_raw_submit_then_wait_for_finalized_success(extrinsic.into())
					.await
			},
		}
		.map(|event| SubmitResponse {
			block_hash: event.block_hash(),
			hash: event.extrinsic_hash(),
			index: event.extrinsic_index(),
		})
		.context("Cannot sign and submit transaction")
	}

	fn has_signer(&self) -> bool {
		self.pair_signer.is_some()
	}
}
