use crate::{network::rpc, types::AvailSecretKey};

use super::types::{SubmitResponse, Transaction};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use avail_subxt::{api, primitives::AvailExtrinsicParams, AvailConfig};
use subxt::{ext::sp_core::sr25519::Pair, tx::PairSigner};

#[async_trait]
pub trait Submit {
	async fn submit(&self, transaction: Transaction) -> Result<SubmitResponse>;
	fn has_signer(&self) -> bool;
}

// TODO: Replace this with avail::PairSigner after implementing required traits in avail-subxt
pub type AvailSigner = PairSigner<AvailConfig, Pair>;

impl From<AvailSecretKey> for AvailSigner {
	fn from(value: AvailSecretKey) -> Self {
		AvailSigner::new(value.0)
	}
}

#[derive(Clone)]
pub struct Submitter {
	pub node_client: rpc::Client,
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
				let extrinsic = api::tx().data_availability().submit_data(data.into());
				let params = AvailExtrinsicParams::new_with_app_id(self.app_id.into());
				self.node_client
					.submit_signed_and_watch(extrinsic, pair_signer.to_owned(), params)
					.await?
			},
			Transaction::Extrinsic(extrinsic) => {
				self.node_client
					.submit_from_bytes_and_watch(extrinsic.into())
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
