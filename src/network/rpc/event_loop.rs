use anyhow::{anyhow, Context, Result};
use avail_subxt::{
	avail, build_client,
	primitives::{grandpa::AuthorityId, Header},
	utils::H256,
	AvailConfig,
};
use codec::Encode;
use futures::Stream;
use kate_recovery::{data::Cell, matrix::Position};
use rocksdb::DB;
use sp_core::{
	blake2_256,
	ed25519::{self, Public},
	Pair,
};
use std::{
	sync::{Arc, Mutex},
	time::Instant,
};
use subxt::{
	rpc::{types::BlockNumber, RpcParams},
	rpc_params, OnlineClient,
};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use tracing::{info, instrument, trace, warn};

use super::{client::Command, ExpectedVersion, Nodes, CELL_WITH_PROOF_SIZE};
use crate::{
	data::store_finality_sync_checkpoint,
	types::{FinalitySyncCheckpoint, GrandpaJustification, RuntimeVersion, SignerMessage, State},
	utils::filter_auth_set_changes,
};

#[derive(Clone, Debug)]
pub enum Event {
	HeaderUpdate {
		header: Header,
		received_at: Instant,
	},
}

enum Subscription {
	Header(Header),
	Justification(GrandpaJustification),
}

struct CurrentValidators {
	set_id: u64,
	validator_set: Vec<Public>,
}

struct BlockData {
	justifications: Vec<GrandpaJustification>,
	unverified_headers: Vec<(Header, Instant)>,
	current_valset: CurrentValidators,
	last_finalized_block_header: Option<Header>,
}

pub struct EventLoop {
	subxt_client: Option<avail::Client>,
	command_receiver: mpsc::Receiver<Command>,
	event_sender: broadcast::Sender<Event>,
	nodes: Nodes,
	db: Arc<DB>,
	state: Arc<Mutex<State>>,
	block_data: BlockData,
}

impl EventLoop {
	pub fn new(
		db: Arc<DB>,
		state: Arc<Mutex<State>>,
		nodes: Nodes,
		command_receiver: mpsc::Receiver<Command>,
		event_sender: broadcast::Sender<Event>,
	) -> EventLoop {
		Self {
			subxt_client: None,
			command_receiver,
			event_sender,
			nodes,
			db,
			state,
			block_data: BlockData {
				justifications: Default::default(),
				unverified_headers: Default::default(),
				current_valset: CurrentValidators {
					set_id: Default::default(),
					validator_set: Default::default(),
				},
				last_finalized_block_header: None,
			},
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

	fn unpack_client(&self) -> Result<&OnlineClient<AvailConfig>> {
		let c = self
			.subxt_client
			.as_ref()
			.ok_or_else(|| anyhow!("RPC client not initialized"))?;
		Ok(c)
	}

	async fn stream_subscriptions(&mut self) -> Result<impl Stream<Item = Subscription>> {
		let client = self.unpack_client()?;
		// create Header subscription
		let header_subscription = client.rpc().subscribe_finalized_block_headers().await?;
		// map Header subscription to the same type for merging
		let header_subscription = header_subscription.filter_map(|s| match s {
			Ok(h) => Some(Subscription::Header(h)),
			Err(_) => None,
		});
		// create Justification subscription
		let justification_subscription = client
			.rpc()
			.subscribe(
				"grandpa_subscribeJustifications",
				rpc_params![],
				"grandpa_unsubscribeJustifications",
			)
			.await?;
		// map Justification subscription to the same type for merging
		let justification_subscription = justification_subscription.filter_map(|s| match s {
			Ok(g) => Some(Subscription::Justification(g)),
			Err(_) => None,
		});

		Ok(header_subscription.merge(justification_subscription))
	}

	async fn gather_block_data(&mut self) -> Result<()> {
		// get the Hash of the Finalized Head
		let last_finalized_block_hash = self.get_chain_head_hash().await?;

		// current Set of Authorities, implicitly trusted, fetched from grandpa runtime.
		let validator_set = self
			.get_validator_set_by_hash(last_finalized_block_hash)
			.await?;
		// fetch the set ID from storage at current height
		let set_id = self.get_set_id_by_hash(last_finalized_block_hash).await?;
		// set Current Valset
		info!("Current set: {:?}", (validator_set.clone(), set_id));
		self.block_data.current_valset = CurrentValidators {
			set_id,
			validator_set,
		};

		// get last (implicitly trusted) Finalized Block Number
		let last_finalized_block_header =
			self.get_header_by_hash(last_finalized_block_hash).await?;
		// set Last Finalized Block Header
		self.block_data.last_finalized_block_header = Some(last_finalized_block_header);

		Ok(())
	}

	pub async fn run(mut self, expected_version: ExpectedVersion<'_>) -> Result<()> {
		// try and create Subxt Client
		self.create_client(expected_version).await?;
		// try to create RPC Subscription Stream
		let mut subscriptions_stream = self.stream_subscriptions().await?;
		// try to get latest Finalized Block Data and set values
		self.gather_block_data().await?;

		loop {
			tokio::select! {
				subscription = subscriptions_stream.next() => self.handle_subscription_stream(subscription.expect("RPC Subscription stream should be infinite")),
				command = self.command_receiver.recv() => match command {
					Some(c) => self.handle_command(c).await,
					// Command channel closed, thus shutting down the RPC Event Loop
					None => return Err(anyhow!("RPC Event Loop shutting down")),
				},
				else => self.output_verified_block_headers().await,
			}
		}
	}

	pub fn subscribe(&mut self) -> BroadcastStream<Event> {
		// convert the broadcast receiver into a RPC Event stream
		let event_receiver = self.event_sender.subscribe();
		BroadcastStream::new(event_receiver)
	}

	fn handle_subscription_stream(&mut self, subscription: Subscription) {
		match subscription {
			Subscription::Header(header) => {
				let received_at = Instant::now();
				self.state.lock().unwrap().latest = header.clone().number;
				info!("Header no.: {}", header.number);
				// push new Unverified Header
				self.block_data
					.unverified_headers
					.push((header.clone(), received_at));

				// search the header logs for validator set change
				let mut new_auths = filter_auth_set_changes(&header);
				// if the event exists, send the new auths over the message channel.
				if !new_auths.is_empty() {
					// TODO: Handle this in a proper fashion
					assert!(
						new_auths.len() == 1,
						"There should be only one valset change!"
					);
					let auths: Vec<(AuthorityId, u64)> = new_auths.pop().unwrap();
					let new_valset = auths
						.into_iter()
						.map(|(a, _)| ed25519::Public::from_raw(a.0 .0 .0))
						.collect::<Vec<Public>>();

					// increment Current Validator Set ID by 1
					self.block_data.current_valset.set_id += 1;
					// set new Validator Set
					self.block_data.current_valset.validator_set = new_valset;
				}
			},
			Subscription::Justification(justification) => {
				info!(
					"New justification at block no.: {}, hash: {:?}",
					justification.commit.target_number, justification.commit.target_hash
				);
				self.block_data.justifications.push(justification);
			},
		}
	}

	async fn output_verified_block_headers(&mut self) {
		while let Some(justification) = self.block_data.justifications.pop() {
			// iterate through Headers and try to find a matching one
			if let Some(pos) = self
				.block_data
				.unverified_headers
				.iter()
				.map(|(h, _)| Encode::using_encoded(h, blake2_256).into())
				.position(|hash| justification.commit.target_hash == hash)
			{
				// basically, pop it out of the collection
				let (header, received_at) = self.block_data.unverified_headers.swap_remove(pos);
				// form a message which is signed in the Justification, it's a triplet of a Precommit,
				// round number and set_id (taken from Substrate code)
				let signed_message = Encode::encode(&(
					&SignerMessage::PrecommitMessage(
						justification.commit.precommits[0].clone().precommit,
					),
					&justification.round,
					&self.block_data.current_valset.set_id, // Set ID is needed here.
				));

				// verify all the Signatures of the Justification signs,
				// verify the hash of the block and extract all the signer addresses
				let signer_addresses = justification
					.commit
					.precommits
					.iter()
					.map(|precommit| {
						let is_ok = <ed25519::Pair as Pair>::verify(
							&precommit.signature,
							&signed_message,
							&precommit.id,
						);
						is_ok
							.then(|| precommit.clone().id)
							.ok_or_else(|| anyhow!("Not signed by this signature!"))
					})
					.collect::<Result<Vec<_>>>();

				let signer_addresses = signer_addresses.unwrap();
				// match all the Signer addresses to the Current Validator Set
				let num_matched_addresses = signer_addresses
					.iter()
					.filter(|x| {
						self.block_data
							.current_valset
							.validator_set
							.iter()
							.any(|e| e.0.eq(&x.0))
					})
					.count();

				info!(
					"Number of matching signatures: {num_matched_addresses}/{} for block {}",
					self.block_data.current_valset.validator_set.len(),
					header.number
				);

				assert!(
					num_matched_addresses
						< self.block_data.current_valset.validator_set.len() * 2 / 3,
					"Not signed by the supermajority of the validator set."
				);

				// store Finality Checkpoint if finality is synced
				let state = self.state.lock().unwrap();
				if !state.finality_synced {
					info!("Storing finality checkpoint at block {}", header.number);
					store_finality_sync_checkpoint(
						self.db.clone(),
						FinalitySyncCheckpoint {
							set_id: self.block_data.current_valset.set_id,
							number: header.number,
							validator_set: self.block_data.current_valset.validator_set.clone(),
						},
					)
					.unwrap();
				}

				// try and get get all the skipped blocks, if they exist
				if let Some(last_header) = self.block_data.last_finalized_block_header.as_ref() {
					for bl_num in (last_header.number + 1)..header.number {
						info!("Sending skipped block {bl_num}");
						let (header, received_at) = match self
							.block_data
							.unverified_headers
							.iter()
							.position(|(h, _)| h.number == bl_num)
						{
							Some(pos) => {
								info!("Fetching header from unverified headers");
								self.block_data.unverified_headers.swap_remove(pos)
							},
							None => {
								info!("Fetching header from RPC");
								(
									self.get_header_by_block_number(bl_num).await.unwrap().0,
									Instant::now(),
								)
							},
						};
						// send as output event
						self.event_sender
							.send(Event::HeaderUpdate {
								header,
								received_at,
							})
							.unwrap();
					}
				}

				info!("Sending finalized block {}", header.number);
				// reset Last Finalized Block Header
				self.block_data.last_finalized_block_header = Some(header.clone());

				// finally, send the Verified Block Header
				self.event_sender
					.send(Event::HeaderUpdate {
						header,
						received_at,
					})
					.unwrap();
			} else {
				trace!("Matched pair of header/justification not found.");
				self.block_data.justifications.push(justification);
				break;
			}
		}
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
		self.unpack_client()?
			.rpc()
			.system_version()
			.await
			.context("Failed to retrieve System version")
	}

	async fn get_runtime_version(&self) -> Result<RuntimeVersion> {
		self.unpack_client()?
			.rpc()
			.request("state_getRuntimeVersion", RpcParams::new())
			.await
			.context("Failed to retrieve Runtime version")
	}

	async fn get_block_hash(&self, block_number: u32) -> Result<H256> {
		self.unpack_client()?
			.rpc()
			.block_hash(Some(BlockNumber::from(block_number)))
			.await?
			.ok_or_else(|| anyhow!("Block with number: {block_number} not found"))
	}

	async fn get_header_by_hash(&self, block_hash: H256) -> Result<Header> {
		self.unpack_client()?
			.rpc()
			.header(Some(block_hash))
			.await?
			.ok_or_else(|| anyhow!("Block Header with hash: {block_hash:?} not found"))
	}

	async fn get_validator_set_by_hash(&self, block_hash: H256) -> Result<Vec<Public>> {
		let valset = self
			.unpack_client()?
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
		let client = self.unpack_client()?;

		let head = client.rpc().finalized_head().await?;

		client
			.rpc()
			.header(Some(head))
			.await?
			.ok_or_else(|| anyhow!("Couldn't get the latest finalized header"))
	}

	async fn get_chain_head_hash(&self) -> Result<H256> {
		self.unpack_client()?
			.rpc()
			.finalized_head()
			.await
			.context("Can not get finalized head hash")
	}

	async fn get_set_id_by_hash(&self, block_hash: H256) -> Result<u64> {
		let set_id_key = avail_subxt::api::storage().grandpa().current_set_id();

		self.unpack_client()?
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
		let mut params = RpcParams::new();
		params.push(rows)?;
		params.push(block_hash)?;

		self.unpack_client()?
			.rpc()
			.request("kate_queryRows", params)
			.await
			.context("Failed to get Kate rows")
	}

	async fn get_kate_proof(&self, positions: &[Position], block_hash: H256) -> Result<Vec<Cell>> {
		let mut params = RpcParams::new();
		params.push(&positions)?;
		params.push(block_hash)?;

		let proofs: Vec<u8> = self
			.unpack_client()?
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
