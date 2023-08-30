use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use avail_subxt::{
	api::{self, runtime_types::sp_core::bounded::bounded_vec::BoundedVec},
	avail,
	primitives::AvailExtrinsicParams,
	AvailConfig,
};
use subxt::{
	ext::sp_core::sr25519::Pair,
	tx::{PairSigner, SubmittableExtrinsic},
};

use super::types::SubmitResponse;

pub enum Transaction {
	Data(Vec<u8>),
	Extrinsic(Vec<u8>),
}

#[async_trait]
pub trait Submit {
	async fn submit(&self, transaction: Transaction) -> Result<SubmitResponse>;
	fn has_signer(&self) -> bool;
}

// TODO: Replace this with avail::PairSigner after implementing required traits in avail-subxt
pub type AvailSigner = PairSigner<AvailConfig, Pair>;

#[derive(Clone)]
pub struct Submitter {
	pub node_client: avail::Client,
	pub app_id: u32,
	pub pair_signer: Option<AvailSigner>,
}

#[async_trait]
impl Submit for Submitter {
	async fn submit(&self, transaction: Transaction) -> Result<SubmitResponse> {
		let tx_progress = match transaction {
			Transaction::Data(data) => {
				let Some(pair_signer) = self.pair_signer.as_ref() else {
					return Err(anyhow!("Pair signer is not configured"));
				};
				let extrinsic = api::tx().data_availability().submit_data(BoundedVec(data));
				let params = AvailExtrinsicParams::new_with_app_id(self.app_id.into());
				self.node_client
					.tx()
					.sign_and_submit_then_watch(&extrinsic, pair_signer, params)
					.await?
			},
			Transaction::Extrinsic(extrinsic) => {
				SubmittableExtrinsic::from_bytes(self.node_client.clone(), extrinsic)
					.submit_and_watch()
					.await?
			},
		};
		tx_progress
			.wait_for_finalized_success()
			.await
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
