#[cfg(feature = "api-v2")]
use avail_subxt::{
	api::data_availability::calls::types::SubmitData, avail::Pair,
	primitives::AvailExtrinsicParams, AvailConfig,
};
#[cfg(feature = "api-v2")]
use subxt::{
	storage::StorageKey,
	tx::{PairSigner, Payload, TxProgress},
	OnlineClient,
};

use anyhow::{Context, Result};
use avail_subxt::{primitives::Header, utils::H256};
use kate_recovery::{data::Cell, matrix::Position};
use sp_core::ed25519::{self, Public};
use subxt::{storage::StorageKey, utils::AccountId32};
use tokio::sync::{mpsc, oneshot};

use super::{Node, WrappedProof};
use crate::types::RuntimeVersion;

#[derive(Clone)]
pub struct Client {
	command_sender: mpsc::Sender<Command>,
}

impl Client {
	pub fn new(command_sender: mpsc::Sender<Command>) -> Self {
		Self { command_sender }
	}

	pub async fn get_block_hash(&self, block_num: u32) -> Result<H256> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetBlockHash {
				block_number: block_num,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_header_by_hash(&self, block_hash: H256) -> Result<Header> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetHeaderByHash {
				block_hash,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_validator_set_by_hash(&self, block_hash: H256) -> Result<Vec<Public>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetValidatorSetByHash {
				block_hash,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_chain_head_header(&self) -> Result<Header> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetChainHeadHeader { response_sender })
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_chain_head_hash(&self) -> Result<H256> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetChainHeadHash { response_sender })
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn request_kate_rows(
		&self,
		rows: Vec<u32>,
		block_hash: H256,
	) -> Result<Vec<Option<Vec<u8>>>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::RequestKateRows {
				rows,
				block_hash,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn request_kate_proof(
		&self,
		block_hash: H256,
		positions: &[Position],
	) -> Result<Vec<Cell>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::RequestKateProof {
				positions: positions.into(),
				block_hash,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_system_version(&self) -> Result<String> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetSystemVersion { response_sender })
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_runtime_version(&self) -> Result<RuntimeVersion> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::RequestRuntimeVersion { response_sender })
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
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
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetConnectedNode { response_sender })
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn fetch_set_id_at(&self, block_hash: H256) -> Result<u64> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::FetchSetIdAt {
				block_hash,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_validator_set_at(&self, block_hash: H256) -> Result<Option<Vec<AccountId32>>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetValidatorSetAt {
				block_hash,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	#[cfg(feature = "api-v2")]
	pub async fn submit_signed_and_watch(
		&self,
		extrinsic: Payload<SubmitData>,
		pair_signer: PairSigner<AvailConfig, Pair>,
		params: AvailExtrinsicParams,
	) -> Result<TxProgress<AvailConfig, OnlineClient<AvailConfig>>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::SubmitSignedAndWatch {
				extrinsic,
				pair_signer,
				params,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	#[cfg(feature = "api-v2")]
	pub async fn submit_from_bytes_and_watch(
		&self,
		tx_bytes: Vec<u8>,
	) -> Result<TxProgress<AvailConfig, OnlineClient<AvailConfig>>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::SubmitFromBytesAndWatch {
				tx_bytes,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_paged_storage_keys(
		&self,
		key: Vec<u8>,
		count: u32,
		start_key: Option<Vec<u8>>,
		hash: Option<H256>,
	) -> Result<Vec<StorageKey>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetPagedStorageKeys {
				key,
				count,
				start_key,
				hash,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_session_key_owner_at(
		&self,
		block_hash: H256,
		public_key: ed25519::Public,
	) -> Result<Option<AccountId32>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetSessionKeyOwnerAt {
				block_hash,
				public_key,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn request_finality_proof(&self, block_number: u32) -> Result<WrappedProof> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::RequestFinalityProof {
				block_number,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_genesis_hash(&self) -> Result<H256> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetGenesisHash { response_sender })
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
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
	#[cfg(feature = "api-v2")]
	SubmitFromBytesAndWatch {
		tx_bytes: Vec<u8>,
		response_sender:
			oneshot::Sender<Result<TxProgress<AvailConfig, OnlineClient<AvailConfig>>>>,
	},
	#[cfg(feature = "api-v2")]
	SubmitSignedAndWatch {
		extrinsic: Payload<SubmitData>,
		pair_signer: PairSigner<AvailConfig, Pair>,
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
