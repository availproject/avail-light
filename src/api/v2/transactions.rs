use anyhow::{Context, Result};
use async_trait::async_trait;
use avail_subxt::{
	api::{self, runtime_types::sp_core::bounded::bounded_vec::BoundedVec},
	avail,
	primitives::AvailExtrinsicParams,
	AvailConfig,
};
use sp_core::H256;
use subxt::{ext::sp_core::sr25519::Pair, tx::PairSigner};

pub enum Transaction {
	Data(Vec<u8>),
	Extrinsic(Vec<u8>),
}

#[async_trait]
pub trait Submit {
	async fn submit(&self, transaction: Transaction) -> Result<H256>;
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
	async fn submit(&self, transaction: Transaction) -> Result<H256> {
		match transaction {
			Transaction::Data(data) => {
				let pair_signer = self
					.pair_signer
					.as_ref()
					.context("Pair signer is not configured")?;
				let data_transfer = api::tx().data_availability().submit_data(BoundedVec(data));
				let extrinsic_params = AvailExtrinsicParams::new_with_app_id(self.app_id.into());
				self.node_client
					.tx()
					.sign_and_submit(&data_transfer, pair_signer, extrinsic_params)
					.await
					.context("Cannot sign and submit transaction")
			},
			Transaction::Extrinsic(data) => self
				.node_client
				.rpc()
				.submit_extrinsic(data)
				.await
				.context("Cannot submit transaction"),
		}
	}

	fn has_signer(&self) -> bool {
		self.pair_signer.is_some()
	}
}
