use avail_subxt::{
	api::{self},
	avail::{self},
	build_client,
	primitives::{grandpa::AuthorityId, Header},
	utils::H256,
	AvailConfig,
};
use codec::Encode;
use color_eyre::{
	eyre::{eyre, Report},
	Result,
};
use futures::{Stream, TryFutureExt, TryStreamExt};
use rocksdb::DB;
use sp_core::{
	blake2_256,
	bytes::from_hex,
	ed25519::{self, Public},
	Pair,
};
use std::{
	pin::Pin,
	sync::{Arc, Mutex},
	time::Instant,
};
use subxt::{
	rpc::{types::BlockNumber, RpcParams},
	rpc_params, OnlineClient,
};
use tokio::sync::{broadcast::Sender, RwLock};
use tokio_retry::Retry;
use tokio_stream::StreamExt;
use tracing::{debug, info, trace, warn};

use super::{CommandReceiver, Node, Nodes, RpcClient, SendableCommand};
use crate::{
	consts::ExpectedNodeVariant,
	data::store_finality_sync_checkpoint,
	types::{
		FinalitySyncCheckpoint, GrandpaJustification, OptionBlockRange, RetryConfig,
		RuntimeVersion, SignerMessage, State, DEV_FLAG_GENHASH,
	},
	utils::filter_auth_set_changes,
};

#[derive(Clone, Debug)]
pub enum Event {
	HeaderUpdate {
		header: Header,
		received_at: Instant,
	},
}

type SubscriptionStream = Pin<Box<dyn Stream<Item = Subscription>>>;

enum Subscription {
	Header(Header),
	Justification(GrandpaJustification),
}

#[derive(Clone, Debug)]
struct ValidatorSet {
	set_id: u64,
	validator_set: Vec<Public>,
}

struct BlockData {
	justifications: Vec<GrandpaJustification>,
	unverified_headers: Vec<(Header, Instant, ValidatorSet)>,
	current_valset: ValidatorSet,
	next_valset: Option<ValidatorSet>,
	last_finalized_block_header: Option<Header>,
}

pub struct EventLoop {
	rpc_client: RpcClient,
	command_receiver: CommandReceiver,
	event_sender: Sender<Event>,
	state: Arc<Mutex<State>>,
	db: Arc<DB>,
	block_data: BlockData,
}

impl EventLoop {
	pub async fn new(
		rpc_client: RpcClient,
		state: Arc<Mutex<State>>,
		db: Arc<DB>,
		command_receiver: CommandReceiver,
		event_sender: Sender<Event>,
	) -> Result<Self> {
		let event_loop = Self {
			rpc_client,
			command_receiver,
			event_sender,
			state,
			db,
			block_data: BlockData {
				justifications: Default::default(),
				unverified_headers: Default::default(),
				current_valset: ValidatorSet {
					set_id: Default::default(),
					validator_set: Default::default(),
				},
				next_valset: None,
				last_finalized_block_header: None,
			},
		};

		Ok(event_loop)
	}

	async fn gather_block_data(&mut self) -> Result<()> {
		// get the Hash of the Finalized Head [with Retries]
		let last_finalized_block_hash = self.get_chain_head_hash().await?;

		// current Set of Authorities, implicitly trusted, fetched from grandpa runtime.
		let validator_set = self
			.get_validator_set_by_hash(last_finalized_block_hash)
			.await?;
		// fetch the set ID from storage at current height [Offline Client; no need for Retries]
		let set_id = self.fetch_set_id_at(last_finalized_block_hash).await?;
		// set Current Valset
		debug!("Current set: {:?}", (validator_set.clone(), set_id));
		self.block_data.current_valset = ValidatorSet {
			set_id,
			validator_set,
		};

		// get last (implicitly trusted) Finalized Block Number [with Retries]
		let last_finalized_block_header =
			self.get_header_by_hash(last_finalized_block_hash).await?;
		// set Last Finalized Block Header
		self.block_data.last_finalized_block_header = Some(last_finalized_block_header);

		Ok(())
	}

	pub async fn run(&mut self) -> Result<()> {
		// create subscriptions stream
		// let subscriptions = self.subscriptions_with_retry().await;
		// futures::pin_mut!(subscriptions);

		loop {
			tokio::select! {
				// subscription = subscriptions.next() => self.handle_subscription_stream(subscription.expect("RPC Subscription stream should be infinite")).await.unwrap(),
				command = self.command_receiver.recv() => match command {
					Some(c) => self.handle_command(c).await,
					// Command channel closed, thus shutting down the RPC Event Loop
					None => return Err(eyre!("RPC Event Loop shutting down")),
				},
			}
		}
	}

	async fn handle_subscription_stream(
		&mut self,
		subscription: Result<Subscription>,
	) -> Result<()> {
		let sub = subscription?;
		match sub {
			Subscription::Header(header) => {
				let received_at = Instant::now();
				self.state.lock().unwrap().latest = header.clone().number;
				info!("Header no.: {}", header.number);

				// if new validator set becomes active, replace the current one
				if self.block_data.next_valset.is_some() {
					self.block_data.current_valset = self.block_data.next_valset.take().unwrap();
				}

				// push new Unverified Header
				self.block_data.unverified_headers.push((
					header.clone(),
					received_at,
					self.block_data.current_valset.clone(),
				));

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

					self.block_data.next_valset = Some(ValidatorSet {
						set_id: self.block_data.current_valset.set_id + 1,
						validator_set: new_valset,
					});

					debug!("Validator set change: {:?}", self.block_data.next_valset);
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
		// check headers
		self.verify_and_output_block_headers().await;

		Ok(())
	}

	async fn verify_and_output_block_headers(&mut self) {
		let mut finality_synced = false;
		while let Some(justification) = self.block_data.justifications.pop() {
			// iterate through Headers and try to find a matching one
			if let Some(pos) = self
				.block_data
				.unverified_headers
				.iter()
				.map(|(h, _, _)| Encode::using_encoded(h, blake2_256).into())
				.position(|hash| justification.commit.target_hash == hash)
			{
				// basically, pop it out of the collection
				let (header, received_at, valset) =
					self.block_data.unverified_headers.swap_remove(pos);
				// form a message which is signed in the Justification, it's a triplet of a Precommit,
				// round number and set_id (taken from Substrate code)
				let signed_message = Encode::encode(&(
					&SignerMessage::PrecommitMessage(
						justification.commit.precommits[0].clone().precommit,
					),
					&justification.round,
					&valset.set_id, // Set ID is needed here.
				));

				// verify all the Signatures of the Justification signs,
				// verify the hash of the block and extract all the signer addresses
				let signer_addresses = justification
					.commit
					.precommits
					.iter()
					.map(|precommit| {
						let mut is_ok = <ed25519::Pair as Pair>::verify(
							&precommit.signature,
							&signed_message,
							&precommit.id,
						);
						if !is_ok {
							warn!("Signature verification fails with default set_id {}, trying alternatives.", valset.set_id);
							for set_id_m in (valset.set_id - 10)..(valset.set_id + 10) {
								let s_m = Encode::encode(&(
									&SignerMessage::PrecommitMessage(
										justification.commit.precommits[0].clone().precommit,
									),
									&justification.round,
									&set_id_m,
								));
								is_ok = <ed25519::Pair as Pair>::verify(
									&precommit.signature,
									&s_m,
									&precommit.id,
								);
								if is_ok {
									info!("Signature match with set_id={set_id_m}");
									break;
								}
							}
						}
						is_ok.then(|| precommit.clone().id).ok_or_else(|| {
							eyre!(
								"Not signed by this signature! Sig id: {:?}, set_id: {}, justification: {:?}",
								&precommit.id,
								valset.set_id,
								justification
							)
						})
					})
					.collect::<Result<Vec<_>>>();

				let signer_addresses = signer_addresses.unwrap();
				// match all the Signer addresses to the Current Validator Set
				let num_matched_addresses = signer_addresses
					.iter()
					.filter(|x| valset.validator_set.iter().any(|e| e.0.eq(&x.0)))
					.count();

				info!(
					"Number of matching signatures: {num_matched_addresses}/{} for block {}, set_id {}",
					valset.validator_set.len(),
					header.number,
					valset.set_id
				);

				assert!(
					is_signed_by_supermajority(num_matched_addresses, valset.validator_set.len()),
					"Not signed by the supermajority of the validator set."
				);

				// To avoid locking the global state all the time, after finality is synced, it will not be necessary to read the state
				if !finality_synced {
					finality_synced = self.state.lock().unwrap().finality_synced;
				}
				// store Finality Checkpoint if finality is synced
				if finality_synced {
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
							.position(|(h, _, _)| h.number == bl_num)
						{
							Some(pos) => {
								info!("Fetching header from unverified headers");
								let p = self.block_data.unverified_headers.swap_remove(pos);
								(p.0, p.1)
							},
							None => {
								info!("Fetching header from RPC");
								let a = self.get_header_by_block_number(bl_num).await.unwrap().0;
								(a, Instant::now())
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
				self.state
					.lock()
					.unwrap()
					.header_verified
					.set(header.number);
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

	async fn handle_command(&self, mut command: SendableCommand) {
		if let Err(err) = self
			.rpc_client
			.with_retries(|client| async move { command.run(&client).await })
			.await
		{
			command.abort(eyre!(err));
		}
	}

	async fn get_block_hash(&self, block_number: u32) -> Result<H256> {
		self.rpc_client
			.with_retries(|client| async move {
				client
					.rpc()
					.block_hash(Some(BlockNumber::from(block_number)))
					.await
					.map_err(Report::from)
			})
			.await?
			.ok_or_else(|| eyre!("Block with number: {block_number} not found"))
	}

	async fn get_header_by_hash(&self, block_hash: H256) -> Result<Header> {
		self.rpc_client
			.with_retries(|client| async move {
				client
					.rpc()
					.header(Some(block_hash))
					.await
					.map_err(Report::from)
			})
			.await?
			.ok_or_else(|| eyre!("Block Header with hash: {block_hash:?} not found"))
	}

	async fn get_validator_set_by_hash(&self, block_hash: H256) -> Result<Vec<Public>> {
		// this call is done with the Offline subxt client variant, no need for retries
		let client = self.rpc_client.current_client().await;

		let valset = client
			.runtime_api()
			.at(block_hash)
			.call_raw::<Vec<(Public, u64)>>("GrandpaApi_grandpa_authorities", None)
			.await?;

		Ok(valset.iter().map(|e| e.0).collect())
	}

	async fn get_chain_head_hash(&self) -> Result<H256> {
		self.rpc_client
			.with_retries(|client| async move {
				client.rpc().finalized_head().await.map_err(Report::from)
			})
			.await
	}

	async fn fetch_set_id_at(&self, block_hash: H256) -> Result<u64> {
		self.rpc_client
			.with_retries(move |client: OnlineClient<AvailConfig>| async move {
				let set_id_key = api::storage().grandpa().current_set_id();
				client
					.storage()
					.at(block_hash)
					.fetch(&set_id_key)
					.await
					.map_err(Report::from)
			})
			.await?
			.ok_or_else(|| eyre!("The set_id should exist"))
	}

	async fn get_header_by_block_number(&self, block_number: u32) -> Result<(Header, H256)> {
		let hash = self.get_block_hash(block_number).await?;
		self.get_header_by_hash(hash).await.map(|e| (e, hash))
	}
}

fn is_signed_by_supermajority(num_signatures: usize, validator_set_size: usize) -> bool {
	let supermajority = (validator_set_size * 2 / 3) + 1;
	num_signatures >= supermajority
}

#[cfg(test)]
mod tests {
	use codec::Encode;
	use hex::FromHex;
	use sp_core::{
		ed25519::{self, Public, Signature},
		Pair,
	};
	use test_case::test_case;

	use crate::types::{Precommit, SignerMessage};
	#[test_case(1, 1 => true)]
	#[test_case(1, 2 => false)]
	#[test_case(2, 2 => true)]
	#[test_case(2, 3 => false)]
	#[test_case(3, 3 => true)]
	#[test_case(3, 4 => true)]
	#[test_case(4, 5 => true)]
	#[test_case(66, 100 => false)]
	#[test_case(67, 100 => true)]
	fn check_supermajority_condition(num_signatures: usize, validator_set_size: usize) -> bool {
		use crate::network::rpc::event_loop::is_signed_by_supermajority;
		is_signed_by_supermajority(num_signatures, validator_set_size)
	}

	#[test_case("019150591418c44041725fc53bbe69fdfb5ec4ad7c35fa3f680db07f41e096988ac3fe0314ca9829fa44fc29e5507bd56f5fa4c45fc955030309bb662f70a10e", "f55c915b3e25a013931f5401a22c3481123584d9ce5a119cabf353bca5c43f05", 41911, "0501c3f8cbba5745aa58ff5f4d8dea89fc2326aa0c95d3eb6fb8070d77511ba9", 14, 9649   => true)]
	#[test_case("b7d22a1854a4836f3d4e7f1af03f8d762913afcf2aa5b20dbdfd23af3e046e80d7410281fdb185b820687a7abe1d201ff866759b00ed2cfc0bab210cea1f7b07", "b91026ef68a88f5ab767a2a7386ac0e7dbb4e62220df1f1c865595bf3afc990b", 39863, "07bc6fca05724fb6cac16fedb80688185ddc74746c7105bddb871cccc626e5e0", 12, 8628   => true)]
	fn check_sig(
		sig_hex: &str,
		target_hash_hex: &str,
		target_number: u32,
		sig_id_hex: &str,
		set_id: u64,
		round: u64,
	) -> bool {
		let sig = Signature(<[u8; 64]>::from_hex(sig_hex).unwrap());
		let precommit_message = Precommit {
			target_hash: <[u8; 32]>::from_hex(target_hash_hex).unwrap().into(),
			target_number,
		};
		let id: Public = Public(<[u8; 32]>::from_hex(sig_id_hex).unwrap());
		let signed_message = Encode::encode(&(
			&SignerMessage::PrecommitMessage(precommit_message),
			&round,
			&set_id,
		));

		<ed25519::Pair as Pair>::verify(&sig, signed_message, &id)
	}
}
