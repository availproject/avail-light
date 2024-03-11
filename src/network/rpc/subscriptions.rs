use avail_subxt::primitives::{grandpa::AuthorityId, Header};
use codec::Encode;
use color_eyre::{eyre::eyre, Result};
use sp_core::{
	blake2_256,
	ed25519::{self, Public},
};
use std::{
	sync::{Arc, Mutex},
	time::Instant,
};
use tokio::sync::broadcast::Sender;
use tokio_stream::StreamExt;
use tracing::{debug, info, trace};

use super::{Client, Subscription};
use crate::{
	data::Database,
	data::{FinalitySyncCheckpoint, Key},
	finality::{check_finality, ValidatorSet},
	types::{GrandpaJustification, OptionBlockRange, State},
	utils::filter_auth_set_changes,
};

#[derive(Clone, Debug)]
pub enum Event {
	HeaderUpdate {
		header: Header,
		received_at: Instant,
	},
}

struct BlockData {
	justifications: Vec<GrandpaJustification>,
	unverified_headers: Vec<(Header, Instant, ValidatorSet)>,
	current_valset: ValidatorSet,
	next_valset: Option<ValidatorSet>,
	last_finalized_block_header: Option<Header>,
}

pub struct SubscriptionLoop<T: Database> {
	rpc_client: Client,
	event_sender: Sender<Event>,
	state: Arc<Mutex<State>>,
	db: T,
	block_data: BlockData,
}

impl<T: Database> SubscriptionLoop<T> {
	pub async fn new(
		state: Arc<Mutex<State>>,
		db: T,
		rpc_client: Client,
		event_sender: Sender<Event>,
	) -> Result<Self> {
		// get the Hash of the Finalized Head [with Retries]
		let last_finalized_block_hash = rpc_client.get_finalized_head_hash().await?;

		// current Set of Authorities, implicitly trusted, fetched from grandpa runtime.
		let validator_set = rpc_client
			.get_validator_set_by_hash(last_finalized_block_hash)
			.await?;
		// fetch the set ID from storage at current height [Offline Client; no need for Retries]
		let set_id = rpc_client
			.fetch_set_id_at(last_finalized_block_hash)
			.await?;
		debug!("Current set: {:?}", (validator_set.clone(), set_id));

		// get last (implicitly trusted) Finalized Block Number [with Retries]
		let last_finalized_block_header = rpc_client
			.get_header_by_hash(last_finalized_block_hash)
			.await?;

		Ok(Self {
			rpc_client,
			event_sender,
			state,
			db,
			block_data: BlockData {
				justifications: Default::default(),
				unverified_headers: Default::default(),
				current_valset: ValidatorSet {
					set_id,
					validator_set,
				},
				next_valset: None,
				last_finalized_block_header: Some(last_finalized_block_header),
			},
		})
	}

	pub async fn run(mut self) -> Result<()> {
		// create subscriptions stream
		let subscriptions = self.rpc_client.clone().subscription_stream().await;
		futures::pin_mut!(subscriptions);

		while let Some(result) = subscriptions.next().await {
			match result {
				Ok(sub) => {
					self.handle_new_subscription(sub).await;
				},
				Err(err) => return Err(eyre!(err)),
			};
		}

		Ok(())
	}

	async fn handle_new_subscription(&mut self, subscription: Subscription) {
		match subscription {
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

				let is_final = check_finality(&valset, &justification);

				is_final.expect("Finality check failed");

				// To avoid locking the global state all the time, after finality is synced, it will not be necessary to read the state
				if !finality_synced {
					finality_synced = self.state.lock().unwrap().finality_synced;
				}
				// store Finality Checkpoint if finality is synced
				if finality_synced {
					info!("Storing finality checkpoint at block {}", header.number);
					self.db
						.put(
							Key::FinalitySyncCheckpoint,
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
								let a = self
									.rpc_client
									.get_header_by_block_number(bl_num)
									.await
									.unwrap()
									.0;
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
}
