use async_trait::async_trait;
use avail_subxt::{
	api::{
		self, data_availability::calls::types::SubmitData,
		runtime_types::sp_core::crypto::KeyTypeId,
	},
	avail::Pair,
	primitives::{AvailExtrinsicParams, Header},
	utils::H256,
	AvailConfig,
};
use color_eyre::{
	eyre::{eyre, WrapErr},
	Report, Result,
};
use kate_recovery::{data::Cell, matrix::Position};
use sp_core::ed25519::{self, Public};
use subxt::{
	rpc::{types::BlockNumber, RpcParams},
	storage::StorageKey,
	tx::{PairSigner, Payload, SubmittableExtrinsic, TxProgress},
	utils::AccountId32,
	OnlineClient,
};
use tokio::sync::oneshot;

use super::{Command, CommandSender, SendableCommand, WrappedProof, CELL_WITH_PROOF_SIZE};
use crate::types::RuntimeVersion;

#[derive(Clone)]
pub struct Client {
	command_sender: CommandSender,
}

struct GetBlockHash {
	block_number: u32,
	response_sender: Option<oneshot::Sender<Result<H256>>>,
}

#[async_trait]
impl Command for GetBlockHash {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let hash = client
			.rpc()
			.block_hash(Some(BlockNumber::from(self.block_number)))
			.await?
			.ok_or_else(|| eyre!("Block with number: {} not found", self.block_number))?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(hash));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct GetHeaderByHash {
	block_hash: H256,
	response_sender: Option<oneshot::Sender<Result<Header>>>,
}

#[async_trait]
impl Command for GetHeaderByHash {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let header = client
			.rpc()
			.header(Some(self.block_hash))
			.await?
			.ok_or_else(|| eyre!("Block Header with hash: {:?} not found", self.block_hash))?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(header));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct GetValidatorSetByHash {
	block_hash: H256,
	response_sender: Option<oneshot::Sender<Result<Vec<Public>>>>,
}

#[async_trait]
impl Command for GetValidatorSetByHash {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let res = client
			.runtime_api()
			.at(self.block_hash)
			.call_raw::<Vec<(Public, u64)>>("GrandpaApi_grandpa_authorities", None)
			.await?
			.iter()
			.map(|e| e.0)
			.collect();

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(res));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

type KateRowsSender = oneshot::Sender<Result<Vec<Option<Vec<u8>>>>>;
struct RequestKateRows {
	rows: Vec<u32>,
	block_hash: H256,
	response_sender: Option<KateRowsSender>,
}

#[async_trait]
impl Command for RequestKateRows {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let mut params = RpcParams::new();
		params.push(self.rows.clone())?;
		params.push(self.block_hash)?;

		let res: Vec<Option<Vec<u8>>> = client.rpc().request("kate_queryRows", params).await?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(res));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct RequestKateProof {
	positions: Vec<Position>,
	block_hash: H256,
	response_sender: Option<oneshot::Sender<Result<Vec<Cell>>>>,
}

#[async_trait]
impl Command for RequestKateProof {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let mut params = RpcParams::new();
		params.push(self.positions.clone())?;
		params.push(self.block_hash)?;

		let proofs: Vec<u8> = client.rpc().request("kate_queryProof", params).await?;

		let i = proofs
			.chunks_exact(CELL_WITH_PROOF_SIZE)
			.map(|chunk| chunk.try_into().expect("chunks of 80 bytes size"));

		let proof = self
			.positions
			.iter()
			.zip(i)
			.map(|(&position, &content)| Cell { position, content })
			.collect::<Vec<_>>();

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(proof));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct GetSystemVersion {
	response_sender: Option<oneshot::Sender<Result<String>>>,
}

#[async_trait]
impl Command for GetSystemVersion {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let res = client.rpc().system_version().await?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(res));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct RequestRuntimeVersion {
	response_sender: Option<oneshot::Sender<Result<RuntimeVersion>>>,
}

#[async_trait]
impl Command for RequestRuntimeVersion {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let res: RuntimeVersion = client
			.rpc()
			.request("state_getRuntimeVersion", RpcParams::new())
			.await?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(res));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct FetchSetIdAt {
	block_hash: H256,
	response_sender: Option<oneshot::Sender<Result<u64>>>,
}

#[async_trait]
impl Command for FetchSetIdAt {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let set_id_key = api::storage().grandpa().current_set_id();
		let res = client
			.storage()
			.at(self.block_hash)
			.fetch(&set_id_key)
			.await?
			.ok_or_else(|| eyre!("The set_id should exist"))?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(res));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct GetValidatorSetAt {
	block_hash: H256,
	response_sender: Option<oneshot::Sender<Result<Option<Vec<AccountId32>>>>>,
}

#[async_trait]
impl Command for GetValidatorSetAt {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		// get validator set from genesis (Substrate Account ID)
		let validators_key = api::storage().session().validators();
		let res = client
			.storage()
			.at(self.block_hash)
			.fetch(&validators_key)
			.await
			.map_err(|e| eyre!(e))?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(res));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

type FromBytesSender = oneshot::Sender<Result<TxProgress<AvailConfig, OnlineClient<AvailConfig>>>>;
struct SubmitFromBytesAndWatch {
	tx_bytes: Vec<u8>,
	response_sender: Option<FromBytesSender>,
}

#[async_trait]
impl Command for SubmitFromBytesAndWatch {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let ext = SubmittableExtrinsic::from_bytes(client.clone(), self.tx_bytes.clone())
			.submit_and_watch()
			.await
			.map_err(|e| eyre!(e))?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(ext));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

type SignedSender = oneshot::Sender<Result<TxProgress<AvailConfig, OnlineClient<AvailConfig>>>>;
struct SubmitSignedAndWatch {
	extrinsic: Payload<SubmitData>,
	pair_signer: PairSigner<AvailConfig, Pair>,
	params: AvailExtrinsicParams,
	response_sender: Option<SignedSender>,
}

#[async_trait]
impl Command for SubmitSignedAndWatch {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let res = client
			.tx()
			.sign_and_submit_then_watch(&self.extrinsic, &self.pair_signer, self.params.clone())
			.await
			.map_err(|e| eyre!(e))?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(res));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct GetPagedStorageKeys {
	key: Vec<u8>,
	count: u32,
	start_key: Option<Vec<u8>>,
	hash: Option<H256>,
	response_sender: Option<oneshot::Sender<Result<Vec<StorageKey>>>>,
}

#[async_trait]
impl Command for GetPagedStorageKeys {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let res = client
			.rpc()
			.storage_keys_paged(&self.key, self.count, self.start_key.as_deref(), self.hash)
			.await
			.map_err(|e| eyre!(e))?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(res));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct GetSessionKeyOwnerAt {
	block_hash: H256,
	public_key: ed25519::Public,
	response_sender: Option<oneshot::Sender<Result<Option<AccountId32>>>>,
}

#[async_trait]
impl Command for GetSessionKeyOwnerAt {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let session_key_key_owner = api::storage().session().key_owner(
			KeyTypeId(sp_core::crypto::key_types::GRANDPA.0),
			self.public_key.0,
		);

		let res = client
			.storage()
			.at(self.block_hash)
			.fetch(&session_key_key_owner)
			.await
			.map_err(|e| eyre!(e))?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(res));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct RequestFinalityProof {
	block_number: u32,
	response_sender: Option<oneshot::Sender<Result<WrappedProof>>>,
}

#[async_trait]
impl Command for RequestFinalityProof {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let mut params = RpcParams::new();
		params.push(self.block_number)?;

		let res: WrappedProof = client
			.rpc()
			.request("grandpa_proveFinality", params)
			.await
			.map_err(|e| eyre!("Request failed at Finality Proof. Error: {e}"))?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(res));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct GetGenesisHash {
	response_sender: Option<oneshot::Sender<Result<H256>>>,
}

#[async_trait]
impl Command for GetGenesisHash {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let res = client.genesis_hash();

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(res));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

struct GetFinalizedHeadHash {
	response_sender: Option<oneshot::Sender<Result<H256>>>,
}

#[async_trait]
impl Command for GetFinalizedHeadHash {
	async fn run(&mut self, client: &avail_subxt::avail::Client) -> Result<()> {
		let head = client.rpc().finalized_head().await?;

		// send result back
		// TODO: consider what to do if this results with None
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Ok(head));
		}
		Ok(())
	}

	fn abort(&mut self, error: Report) {
		// TODO: consider what to do if this results with None
		// TODO: maybe wrap error with specific error message per Command type implementation
		if let Some(sender) = self.response_sender.take() {
			_ = sender.send(Err(error));
		}
	}
}

// struct GetConnectedNode {
// 	response_sender: oneshot::Sender<Result<Node>>,
// }

impl Client {
	pub fn new(command_sender: CommandSender) -> Self {
		Self { command_sender }
	}

	async fn execute_sync<F, T>(&self, command_with_sender: F) -> Result<T>
	where
		F: FnOnce(oneshot::Sender<Result<T>>) -> SendableCommand,
	{
		let (response_sender, response_receiver) = oneshot::channel();
		let command = command_with_sender(response_sender);
		self.command_sender.send(command).await?;
		response_receiver
			.await
			.context("sender should not be dropped")?
	}

	pub async fn get_block_hash(&self, block_number: u32) -> Result<H256> {
		self.execute_sync(|response_sender| {
			Box::new(GetBlockHash {
				block_number,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn get_header_by_hash(&self, block_hash: H256) -> Result<Header> {
		self.execute_sync(|response_sender| {
			Box::new(GetHeaderByHash {
				block_hash,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn get_validator_set_by_hash(&self, block_hash: H256) -> Result<Vec<Public>> {
		self.execute_sync(|response_sender| {
			Box::new(GetValidatorSetByHash {
				block_hash,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn get_finalized_head_hash(&self) -> Result<H256> {
		self.execute_sync(|response_sender| {
			Box::new(GetFinalizedHeadHash {
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn get_chain_head_header(&self) -> Result<Header> {
		let finalized_hash = self.get_finalized_head_hash().await?;
		self.get_header_by_hash(finalized_hash).await
	}

	pub async fn request_kate_rows(
		&self,
		rows: Vec<u32>,
		block_hash: H256,
	) -> Result<Vec<Option<Vec<u8>>>> {
		self.execute_sync(|response_sender| {
			Box::new(RequestKateRows {
				rows,
				block_hash,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn request_kate_proof(
		&self,
		block_hash: H256,
		positions: &[Position],
	) -> Result<Vec<Cell>> {
		self.execute_sync(|response_sender| {
			Box::new(RequestKateProof {
				positions: positions.into(),
				block_hash,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn get_system_version(&self) -> Result<String> {
		self.execute_sync(|response_sender| {
			Box::new(GetSystemVersion {
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn get_runtime_version(&self) -> Result<RuntimeVersion> {
		self.execute_sync(|response_sender| {
			Box::new(RequestRuntimeVersion {
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn get_validator_set_by_block_number(&self, block_num: u32) -> Result<Vec<Public>> {
		let hash = self.get_block_hash(block_num).await?;
		self.get_validator_set_by_hash(hash).await
	}

	pub async fn fetch_set_id_at(&self, block_hash: H256) -> Result<u64> {
		self.execute_sync(|response_sender| {
			Box::new(FetchSetIdAt {
				block_hash,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn get_current_set_id_by_block_number(&self, block_num: u32) -> Result<u64> {
		let hash = self.get_block_hash(block_num).await?;
		self.fetch_set_id_at(hash).await
	}

	pub async fn get_header_by_block_number(&self, block_num: u32) -> Result<(Header, H256)> {
		let hash = self.get_block_hash(block_num).await?;
		self.get_header_by_hash(hash)
			.await
			.map(|header| (header, hash))
	}

	pub async fn get_validator_set_at(&self, block_hash: H256) -> Result<Option<Vec<AccountId32>>> {
		self.execute_sync(|response_sender| {
			Box::new(GetValidatorSetAt {
				block_hash,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn submit_signed_and_watch(
		&self,
		extrinsic: Payload<SubmitData>,
		pair_signer: PairSigner<AvailConfig, Pair>,
		params: AvailExtrinsicParams,
	) -> Result<TxProgress<AvailConfig, OnlineClient<AvailConfig>>> {
		self.execute_sync(|response_sender| {
			Box::new(SubmitSignedAndWatch {
				extrinsic,
				pair_signer,
				params,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn submit_from_bytes_and_watch(
		&self,
		tx_bytes: Vec<u8>,
	) -> Result<TxProgress<AvailConfig, OnlineClient<AvailConfig>>> {
		self.execute_sync(|response_sender| {
			Box::new(SubmitFromBytesAndWatch {
				tx_bytes,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn get_paged_storage_keys(
		&self,
		key: Vec<u8>,
		count: u32,
		start_key: Option<Vec<u8>>,
		hash: Option<H256>,
	) -> Result<Vec<StorageKey>> {
		self.execute_sync(|response_sender| {
			Box::new(GetPagedStorageKeys {
				key,
				count,
				start_key,
				hash,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn get_session_key_owner_at(
		&self,
		block_hash: H256,
		public_key: ed25519::Public,
	) -> Result<Option<AccountId32>> {
		self.execute_sync(|response_sender| {
			Box::new(GetSessionKeyOwnerAt {
				block_hash,
				public_key,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn request_finality_proof(&self, block_number: u32) -> Result<WrappedProof> {
		self.execute_sync(|response_sender| {
			Box::new(RequestFinalityProof {
				block_number,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn get_genesis_hash(&self) -> Result<H256> {
		self.execute_sync(|response_sender| {
			Box::new(GetGenesisHash {
				response_sender: Some(response_sender),
			})
		})
		.await
	}
}
