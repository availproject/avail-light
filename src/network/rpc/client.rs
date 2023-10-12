use anyhow::{Context, Result};
use avail_subxt::{primitives::Header as DaHeader, utils::H256};
use kate_recovery::{data::Cell, matrix::Position};
use sp_core::ed25519::Public;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::types::RuntimeVersionResult;

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
				block_num,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_header_by_hash(&self, block_hash: H256) -> Result<DaHeader> {
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

	pub async fn get_chain_head_header(&self) -> Result<DaHeader> {
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

	pub async fn get_current_set_id_by_hash(&self, block_hash: H256) -> Result<u64> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetCurrentSetIdByHash {
				block_hash,
				response_sender,
			})
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}

	pub async fn get_kate_rows(
		&self,
		rows: Vec<u32>,
		block_hash: H256,
	) -> Result<Vec<Option<Vec<u8>>>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetKateRows {
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

	pub async fn get_kate_proof(
		&self,
		positions: &[Position],
		block_hash: H256,
	) -> Result<Vec<Cell>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetKateProof {
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

	pub async fn get_runtime_version(&self) -> Result<RuntimeVersionResult> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetRuntimeVersion { response_sender })
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
		self.get_current_set_id_by_hash(hash).await
	}

	pub async fn get_header_by_block_number(&self, block_num: u32) -> Result<(DaHeader, H256)> {
		let hash = self.get_block_hash(block_num).await?;
		self.get_header_by_hash(hash)
			.await
			.map(|header| (header, hash))
	}
}

pub enum Command {
	GetBlockHash {
		block_num: u32,
		response_sender: oneshot::Sender<Result<H256>>,
	},
	GetHeaderByHash {
		block_hash: H256,
		response_sender: oneshot::Sender<Result<DaHeader>>,
	},
	GetValidatorSetByHash {
		block_hash: H256,
		response_sender: oneshot::Sender<Result<Vec<Public>>>,
	},
	GetValidatorSetByBlockNumber {
		block_num: u32,
		response_sender: oneshot::Sender<Result<Vec<Public>>>,
	},
	GetChainHeadHeader {
		response_sender: oneshot::Sender<Result<DaHeader>>,
	},
	GetChainHeadHash {
		response_sender: oneshot::Sender<Result<H256>>,
	},
	GetCurrentSetIdByHash {
		block_hash: H256,
		response_sender: oneshot::Sender<Result<u64>>,
	},
	GetKateRows {
		rows: Vec<u32>,
		block_hash: H256,
		response_sender: oneshot::Sender<Result<Vec<Option<Vec<u8>>>>>,
	},
	GetKateProof {
		positions: Arc<[Position]>,
		block_hash: H256,
		response_sender: oneshot::Sender<Result<Vec<Cell>>>,
	},
	GetSystemVersion {
		response_sender: oneshot::Sender<Result<String>>,
	},
	GetRuntimeVersion {
		response_sender: oneshot::Sender<Result<RuntimeVersionResult>>,
	},
}
