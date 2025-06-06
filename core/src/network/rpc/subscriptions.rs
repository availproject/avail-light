use avail_rust::AvailHeader;
use codec::Encode;
use color_eyre::{eyre::eyre, Result};
use sp_core::ed25519::{self, Public};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast::Sender;
use tokio_stream::StreamExt;
use tracing::{debug, info, trace};
#[cfg(target_arch = "wasm32")]
use web_time::{Instant, SystemTime, UNIX_EPOCH};

use super::{Client, OutputEvent, Subscription};
use crate::{
	data::{
		BlockHeaderReceivedAtKey, Database, FinalitySyncCheckpoint, FinalitySyncCheckpointKey,
		IsFinalitySyncedKey, LatestHeaderKey, VerifiedHeaderKey,
	},
	finality::{check_finality, ValidatorSet},
	types::{BlockRange, GrandpaJustification},
	utils::{blake2_256, filter_auth_set_changes},
};

struct BlockData {
	justifications: Vec<GrandpaJustification>,
	unverified_headers: Vec<(AvailHeader, Instant, ValidatorSet)>,
	current_valset: ValidatorSet,
	next_valset: Option<ValidatorSet>,
	last_finalized_block_header: Option<AvailHeader>,
}

pub struct SubscriptionLoop<T: Database> {
	rpc_client: Client<T>,
	event_sender: Sender<OutputEvent>,
	db: T,
	block_data: BlockData,
}

impl<T: Database + Clone> SubscriptionLoop<T> {
	pub async fn new(
		db: T,
		rpc_client: Client<T>,
		event_sender: Sender<OutputEvent>,
	) -> Result<Self> {
		// get the Hash of the Finalized Head [with Retries]
		let last_finalized_block_hash = rpc_client.get_finalized_head_hash().await?;

		// current Set of Authorities, implicitly trusted, fetched from grandpa runtime [with Retries].
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

		loop {
			match subscriptions.next().await {
				Some(Ok(sub)) => self.handle_new_subscription(sub).await,
				Some(Err(error)) => return Err(eyre!(error)),
				None => return Err(eyre!("No available subscriptions")),
			}
		}
	}

	async fn handle_new_subscription(&mut self, subscription: Subscription) {
		match subscription {
			Subscription::Header(header) => {
				let received_at = Instant::now();
				let received_at_timestamp = SystemTime::now()
					.duration_since(UNIX_EPOCH)
					.expect("Time went backwards")
					.as_secs();
				self.db.put(LatestHeaderKey, header.clone().number);
				info!("Header no.: {}", header.number);

				self.db.put(
					BlockHeaderReceivedAtKey(header.number),
					received_at_timestamp,
				);

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
				let new_auths = filter_auth_set_changes(&header);
				// if the event exists, send the new auths over the message channel.
				if !new_auths.is_empty() {
					let new_valset = new_auths
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

				let received_at_timestamp = self
					.db
					.get(BlockHeaderReceivedAtKey(header.number))
					.expect("Block header timestamp is in the database");

				let is_final = check_finality(&valset, &justification);

				is_final.expect("Finality check failed");

				// store Finality Checkpoint if finality is synced
				if finality_synced {
					info!("Storing finality checkpoint at block {}", header.number);
					self.db.put(
						FinalitySyncCheckpointKey,
						FinalitySyncCheckpoint {
							set_id: self.block_data.current_valset.set_id,
							number: header.number,
							validator_set: self.block_data.current_valset.validator_set.clone(),
						},
					);
				} else {
					// to avoid reading from db all the time,
					// after finality is synced, it will not be necessary to read the state

					finality_synced = self.db.get(IsFinalitySyncedKey).unwrap_or(false)
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
							.send(OutputEvent::HeaderUpdate {
								header,
								received_at,
								received_at_timestamp,
							})
							.unwrap();
					}
				}

				info!("Sending finalized block {}", header.number);
				// reset Last Finalized Block Header
				self.block_data.last_finalized_block_header = Some(header.clone());

				// get currently stored Verified Header
				let mut verified_header = self
					.db
					.get(VerifiedHeaderKey)
					.unwrap_or_else(|| BlockRange::init(header.number));
				// mutate value
				verified_header.last = header.number;
				// and store in DB
				self.db.put(VerifiedHeaderKey, verified_header);

				// finally, send the Verified Block Header
				self.event_sender
					.send(OutputEvent::HeaderUpdate {
						header,
						received_at,
						received_at_timestamp,
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
