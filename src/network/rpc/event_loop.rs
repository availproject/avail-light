use anyhow::{anyhow, Context, Result};
use avail_subxt::{avail, build_client, primitives::Header, utils::H256};
use kate_recovery::{data::Cell, matrix::Position};
use sp_core::ed25519::Public;
use std::time::Instant;
use subxt::rpc::{types::BlockNumber, RpcParams};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::BroadcastStream;
use tracing::{info, instrument, warn};

use super::{client::Command, ExpectedVersion, Nodes, CELL_WITH_PROOF_SIZE};
use crate::types::RuntimeVersion;

#[derive(Clone)]
pub enum Event {
	HeaderUpdate { header: Header, instant: Instant },
}

pub struct EventLoop {
	subxt_client: Option<avail::Client>,
	command_receiver: mpsc::Receiver<Command>,
	event_sender: broadcast::Sender<Event>,
	nodes: Nodes,
}

impl EventLoop {
	pub fn new(
		nodes: Nodes,
		command_receiver: mpsc::Receiver<Command>,
		event_sender: broadcast::Sender<Event>,
	) -> EventLoop {
		Self {
			subxt_client: None,
			command_receiver,
			event_sender,
			nodes,
		}
	}

	async fn create_client(&mut self, expected_version: ExpectedVersion<'_>) -> Result<()> {
		// shuffle passed Nodes and start try to connect the first one
		let node = self
			.nodes
			.reset()
			.ok_or_else(|| anyhow!("RPC WS Nodes list must not be empty"))?;

		let log_warn = |error| {
			warn!("Skipping connection to {:?}: {error}", node.host);
			error
		};

		let client = build_client(&node.host, false).await?;
		// client was built successfully, keep it
		self.subxt_client.replace(client);
		let system_version = self.get_system_version().await?;
		let runtime_version = self.get_runtime_version().await?;

		let version = format!(
			"v/{}/{}/{}",
			system_version, runtime_version.spec_name, runtime_version.spec_version
		);

		if !expected_version.matches(&system_version, &runtime_version.spec_name) {
			log_warn(anyhow!("Expected {expected_version}, found {version}"));
		}

		info!(
			"Connection established to the Node: {:?} <{version}>",
			node.host
		);

		Ok(())
	}

	pub async fn run(mut self, expected_version: ExpectedVersion<'_>) -> Result<()> {
		self.create_client(expected_version).await?;

		loop {
			tokio::select! {
				command = self.command_receiver.recv() => match command {
					Some(c) => self.handle_command(c).await,
					// Command channel closed, thus shutting down the RPC Event Loop
					None => return Err(anyhow!("RPC Event Loop shutting down")),
				}
			}
		}
	}

	pub fn subscribe(&self) -> BroadcastStream<Event> {
		// convert the broadcast receiver into a RPC Event stream
		let event_receiver = self.event_sender.subscribe();
		BroadcastStream::new(event_receiver)
	}

	async fn handle_command(&self, command: Command) {
		match command {
			Command::GetSystemVersion { response_sender } => {
				let res = self.get_system_version().await;
				_ = response_sender.send(res);
			},
			Command::GetRuntimeVersion { response_sender } => {
				let res = self.get_runtime_version().await;
				_ = response_sender.send(res);
			},
			Command::GetBlockHash {
				block_num,
				response_sender,
			} => {
				let res = self.get_block_hash(block_num).await;
				_ = response_sender.send(res);
			},
			Command::GetHeaderByHash {
				block_hash,
				response_sender,
			} => {
				let res = self.get_header_by_hash(block_hash).await;
				_ = response_sender.send(res);
			},
			Command::GetValidatorSetByHash {
				block_hash,
				response_sender,
			} => {
				let res = self.get_validator_set_by_hash(block_hash).await;
				_ = response_sender.send(res);
			},
			Command::GetValidatorSetByBlockNumber {
				block_num,
				response_sender,
			} => {
				let res = self.get_validator_set_by_block_number(block_num).await;
				_ = response_sender.send(res);
			},
			Command::GetChainHeadHeader { response_sender } => {
				let res = self.get_chain_head_header().await;
				_ = response_sender.send(res);
			},
			Command::GetChainHeadHash { response_sender } => {
				let res = self.get_chain_head_hash().await;
				_ = response_sender.send(res)
			},
			Command::GetCurrentSetIdByHash {
				block_hash,
				response_sender,
			} => {
				let res = self.get_set_id_by_hash(block_hash).await;
				_ = response_sender.send(res);
			},
			Command::GetCurrentSetIdByBlockNumber {
				block_number,
				response_sender,
			} => {
				let res = self.get_set_id_by_block_number(block_number).await;
				_ = response_sender.send(res);
			},
			Command::GetHeaderByBlockNumber {
				block_number,
				response_sender,
			} => {
				let res = self.get_header_by_block_number(block_number).await;
				_ = response_sender.send(res);
			},
			Command::GetKateRows {
				rows,
				block_hash,
				response_sender,
			} => {
				let res = self.get_kate_rows(rows, block_hash).await;
				_ = response_sender.send(res);
			},
			Command::GetKateProof {
				positions,
				block_hash,
				response_sender,
			} => {
				let res = self.get_kate_proof(&positions, block_hash).await;
				_ = response_sender.send(res)
			},
		}
	}

	async fn get_system_version(&self) -> Result<String> {
		let Some(client) = &self.subxt_client else { return  Err(anyhow!("RPC client not initialized"))};

		client
			.rpc()
			.system_version()
			.await
			.context("Failed to retrieve System version")
	}

	async fn get_runtime_version(&self) -> Result<RuntimeVersion> {
		let Some(client) = &self.subxt_client else {return Err(anyhow!("RPC client not initialized"))};

		client
			.rpc()
			.request("state_getRuntimeVersion", RpcParams::new())
			.await
			.context("Failed to retrieve Runtime version")
	}

	async fn get_block_hash(&self, block_number: u32) -> Result<H256> {
		let Some(client) = &self.subxt_client else { return  Err(anyhow!("RPC client not initialized"))};

		client
			.rpc()
			.block_hash(Some(BlockNumber::from(block_number)))
			.await?
			.ok_or_else(|| anyhow!("Block with number: {block_number} not found"))
	}

	async fn get_header_by_hash(&self, block_hash: H256) -> Result<Header> {
		let Some(client) = &self.subxt_client else { return  Err(anyhow!("RPC client not initialized"))};

		client
			.rpc()
			.header(Some(block_hash))
			.await?
			.ok_or_else(|| anyhow!("Block Header with hash: {block_hash:?} not found"))
	}

	async fn get_validator_set_by_hash(&self, block_hash: H256) -> Result<Vec<Public>> {
		let Some(client) = &self.subxt_client else { return  Err(anyhow!("RPC client not initialized"))};

		let valset = client
			.runtime_api()
			.at(block_hash)
			.call_raw::<Vec<(Public, u64)>>("GrandpaApi_grandpa_authorities", None)
			.await?;

		Ok(valset.iter().map(|e| e.0).collect())
	}

	async fn get_validator_set_by_block_number(&self, block_number: u32) -> Result<Vec<Public>> {
		let block_hash = self.get_block_hash(block_number).await?;
		self.get_validator_set_by_hash(block_hash).await
	}

	async fn get_chain_head_header(&self) -> Result<Header> {
		let Some(client) = &self.subxt_client else { return Err(anyhow!("RPC client not initialized"))};

		let head = client.rpc().finalized_head().await?;
		client
			.rpc()
			.header(Some(head))
			.await?
			.ok_or_else(|| anyhow!("Couldn't get the latest finalized header"))
	}

	async fn get_chain_head_hash(&self) -> Result<H256> {
		let Some(client) = &self.subxt_client else { return Err(anyhow!("RPC client not initialized"))};

		client
			.rpc()
			.finalized_head()
			.await
			.context("Can not get finalized head hash")
	}

	async fn get_set_id_by_hash(&self, block_hash: H256) -> Result<u64> {
		let Some(client) = &self.subxt_client else { return Err(anyhow!("RPC client not initialized"))};

		let set_id_key = avail_subxt::api::storage().grandpa().current_set_id();
		client
			.storage()
			.at(block_hash)
			.fetch(&set_id_key)
			.await?
			.ok_or_else(|| anyhow!("The set_id should exist"))
	}

	async fn get_set_id_by_block_number(&self, block_number: u32) -> Result<u64> {
		let block_hash = self.get_block_hash(block_number).await?;
		self.get_set_id_by_hash(block_hash).await
	}

	async fn get_header_by_block_number(&self, block_number: u32) -> Result<(Header, H256)> {
		let hash = self.get_block_hash(block_number).await?;
		self.get_header_by_hash(hash).await.map(|e| (e, hash))
	}

	#[instrument(skip_all, level = "trace")]
	async fn get_kate_rows(
		&self,
		rows: Vec<u32>,
		block_hash: H256,
	) -> Result<Vec<Option<Vec<u8>>>> {
		let Some(client) = &self.subxt_client else { return Err(anyhow!("RPC client not initialized"))};

		let mut params = RpcParams::new();
		params.push(rows)?;
		params.push(block_hash)?;

		client
			.rpc()
			.request("kate_queryRows", params)
			.await
			.context("Failed to get Kate rows")
	}

	async fn get_kate_proof(&self, positions: &[Position], block_hash: H256) -> Result<Vec<Cell>> {
		let Some(client) = &self.subxt_client else { return Err(anyhow!("RPC client not initialized"))};

		let mut params = RpcParams::new();
		params.push(&positions)?;
		params.push(block_hash)?;

		let proofs: Vec<u8> = client
			.rpc()
			.request("kate_queryProof", params)
			.await
			.context("Failed to fetch proofs")?;

		let i = proofs
			.chunks_exact(CELL_WITH_PROOF_SIZE)
			.map(|chunk| chunk.try_into().expect("chunks of 80 bytes size"));

		Ok(positions
			.iter()
			.zip(i)
			.map(|(&position, &content)| Cell { position, content })
			.collect::<Vec<_>>())
	}
}
