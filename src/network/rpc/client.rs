use anyhow::{Context, Result};
use avail_subxt::{
	api::data_availability::calls::types::SubmitData,
	avail::Pair,
	primitives::{AvailExtrinsicParams, Header},
	utils::H256,
	AvailConfig,
};
use kate_recovery::{data::Cell, matrix::Position};
use sp_core::ed25519::{self, Public};
use subxt::{
	storage::StorageKey,
	tx::{PairSigner, Payload, TxProgress},
	utils::AccountId32,
	OnlineClient,
};
use tokio::sync::{mpsc, oneshot};

use super::{Node, WrappedProof};
use crate::types::RuntimeVersion;

#[derive(Clone)]
pub struct Client {
	command_sender: mpsc::Sender<Command>,
}

// Takes command sender and function that creates command from response sender,
// and sends command to command sender, waiting on response receiver for result
async fn execute_sync<F, C, T>(sender: &mpsc::Sender<C>, command_fn: F) -> anyhow::Result<T>
where
	C: Send + Sync + 'static,
	F: FnOnce(oneshot::Sender<Result<T>>) -> C,
{
	let (response_sender, response_receiver) = oneshot::channel();
	sender
		.send(command_fn(response_sender))
		.await
		.context("receiver should not be dropped")?;
	response_receiver
		.await
		.context("sender should not be dropped")?
}

impl Client {
	pub fn new(command_sender: mpsc::Sender<Command>) -> Self {
		Self { command_sender }
	}

	async fn execute_sync<F, T>(&self, command_fn: F) -> anyhow::Result<T>
	where
		F: FnOnce(oneshot::Sender<Result<T>>) -> Command,
	{
		execute_sync(&self.command_sender, command_fn).await
	}

	pub async fn get_block_hash(&self, block_number: u32) -> Result<H256> {
		self.execute_sync(|response_sender| Command::GetBlockHash {
			block_number,
			response_sender,
		})
		.await
	}

	pub async fn get_header_by_hash(&self, block_hash: H256) -> Result<Header> {
		self.execute_sync(|response_sender| Command::GetHeaderByHash {
			block_hash,
			response_sender,
		})
		.await
	}

	pub async fn get_validator_set_by_hash(&self, block_hash: H256) -> Result<Vec<Public>> {
		self.execute_sync(|response_sender| Command::GetValidatorSetByHash {
			block_hash,
			response_sender,
		})
		.await
	}

	pub async fn get_chain_head_header(&self) -> Result<Header> {
		self.execute_sync(|response_sender| Command::GetChainHeadHeader { response_sender })
			.await
	}

	pub async fn get_chain_head_hash(&self) -> Result<H256> {
		self.execute_sync(|response_sender| Command::GetChainHeadHash { response_sender })
			.await
	}

	pub async fn request_kate_rows(
		&self,
		rows: Vec<u32>,
		block_hash: H256,
	) -> Result<Vec<Option<Vec<u8>>>> {
		self.execute_sync(|response_sender| Command::RequestKateRows {
			rows,
			block_hash,
			response_sender,
		})
		.await
	}

	pub async fn request_kate_proof(
		&self,
		block_hash: H256,
		positions: &[Position],
	) -> Result<Vec<Cell>> {
		self.execute_sync(|response_sender| Command::RequestKateProof {
			positions: positions.into(),
			block_hash,
			response_sender,
		})
		.await
	}

	pub async fn get_system_version(&self) -> Result<String> {
		self.execute_sync(|response_sender| Command::GetSystemVersion { response_sender })
			.await
	}

	pub async fn get_runtime_version(&self) -> Result<RuntimeVersion> {
		self.execute_sync(|response_sender| Command::RequestRuntimeVersion { response_sender })
			.await
	}

	pub async fn get_validator_set_by_block_number(&self, block_num: u32) -> Result<Vec<Public>> {
		let hash = self.get_block_hash(block_num).await?;
		self.get_validator_set_by_hash(hash).await
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

	pub async fn get_connected_node(&self) -> Result<Node> {
		self.execute_sync(|response_sender| Command::GetConnectedNode { response_sender })
			.await
	}

	pub async fn fetch_set_id_at(&self, block_hash: H256) -> Result<u64> {
		self.execute_sync(|response_sender| Command::FetchSetIdAt {
			block_hash,
			response_sender,
		})
		.await
	}

	pub async fn get_validator_set_at(&self, block_hash: H256) -> Result<Option<Vec<AccountId32>>> {
		self.execute_sync(|response_sender| Command::GetValidatorSetAt {
			block_hash,
			response_sender,
		})
		.await
	}

	pub async fn submit_signed_and_watch(
		&self,
		extrinsic: Payload<SubmitData>,
		pair_signer: PairSigner<AvailConfig, Pair>,
		params: AvailExtrinsicParams,
	) -> Result<TxProgress<AvailConfig, OnlineClient<AvailConfig>>> {
		self.execute_sync(|response_sender| Command::SubmitSignedAndWatch {
			extrinsic,
			pair_signer: Box::new(pair_signer),
			params,
			response_sender,
		})
		.await
	}

	pub async fn submit_from_bytes_and_watch(
		&self,
		tx_bytes: Vec<u8>,
	) -> Result<TxProgress<AvailConfig, OnlineClient<AvailConfig>>> {
		self.execute_sync(|response_sender| Command::SubmitFromBytesAndWatch {
			tx_bytes,
			response_sender,
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
		self.execute_sync(|response_sender| Command::GetPagedStorageKeys {
			key,
			count,
			start_key,
			hash,
			response_sender,
		})
		.await
	}

	pub async fn get_session_key_owner_at(
		&self,
		block_hash: H256,
		public_key: ed25519::Public,
	) -> Result<Option<AccountId32>> {
		self.execute_sync(|response_sender| Command::GetSessionKeyOwnerAt {
			block_hash,
			public_key,
			response_sender,
		})
		.await
	}

	pub async fn request_finality_proof(&self, block_number: u32) -> Result<WrappedProof> {
		self.execute_sync(|response_sender| Command::RequestFinalityProof {
			block_number,
			response_sender,
		})
		.await
	}

	pub async fn get_genesis_hash(&self) -> Result<H256> {
		self.execute_sync(|response_sender| Command::GetGenesisHash { response_sender })
			.await
	}
}

pub enum Command {
	GetBlockHash {
		block_number: u32,
		response_sender: oneshot::Sender<Result<H256>>,
	},
	GetHeaderByHash {
		block_hash: H256,
		response_sender: oneshot::Sender<Result<Header>>,
	},
	GetValidatorSetByHash {
		block_hash: H256,
		response_sender: oneshot::Sender<Result<Vec<Public>>>,
	},
	GetValidatorSetByBlockNumber {
		block_number: u32,
		response_sender: oneshot::Sender<Result<Vec<Public>>>,
	},
	GetChainHeadHeader {
		response_sender: oneshot::Sender<Result<Header>>,
	},
	GetChainHeadHash {
		response_sender: oneshot::Sender<Result<H256>>,
	},
	GetCurrentSetIdByBlockNumber {
		block_number: u32,
		response_sender: oneshot::Sender<Result<u64>>,
	},
	GetHeaderByBlockNumber {
		block_number: u32,
		response_sender: oneshot::Sender<Result<(Header, H256)>>,
	},
	RequestKateRows {
		rows: Vec<u32>,
		block_hash: H256,
		response_sender: oneshot::Sender<Result<Vec<Option<Vec<u8>>>>>,
	},
	RequestKateProof {
		positions: Vec<Position>,
		block_hash: H256,
		response_sender: oneshot::Sender<Result<Vec<Cell>>>,
	},
	GetSystemVersion {
		response_sender: oneshot::Sender<Result<String>>,
	},
	RequestRuntimeVersion {
		response_sender: oneshot::Sender<Result<RuntimeVersion>>,
	},
	GetConnectedNode {
		response_sender: oneshot::Sender<Result<Node>>,
	},
	FetchSetIdAt {
		block_hash: H256,
		response_sender: oneshot::Sender<Result<u64>>,
	},
	GetValidatorSetAt {
		block_hash: H256,
		response_sender: oneshot::Sender<Result<Option<Vec<AccountId32>>>>,
	},
	SubmitFromBytesAndWatch {
		tx_bytes: Vec<u8>,
		response_sender:
			oneshot::Sender<Result<TxProgress<AvailConfig, OnlineClient<AvailConfig>>>>,
	},
	SubmitSignedAndWatch {
		extrinsic: Payload<SubmitData>,
		pair_signer: Box<PairSigner<AvailConfig, Pair>>,
		params: AvailExtrinsicParams,
		response_sender:
			oneshot::Sender<Result<TxProgress<AvailConfig, OnlineClient<AvailConfig>>>>,
	},
	GetPagedStorageKeys {
		key: Vec<u8>,
		count: u32,
		start_key: Option<Vec<u8>>,
		hash: Option<H256>,
		response_sender: oneshot::Sender<Result<Vec<StorageKey>>>,
	},
	GetSessionKeyOwnerAt {
		block_hash: H256,
		public_key: ed25519::Public,
		response_sender: oneshot::Sender<Result<Option<AccountId32>>>,
	},
	RequestFinalityProof {
		block_number: u32,
		response_sender: oneshot::Sender<Result<WrappedProof>>,
	},
	GetGenesisHash {
		response_sender: oneshot::Sender<Result<H256>>,
	},
}
