use std::time::Instant;

use anyhow::{anyhow, Result};
use avail_subxt::{
	avail::Client,
	primitives::{grandpa::AuthorityId, Header},
	rpc::rpc_params,
	utils::H256,
};
use codec::{Decode, Encode};
use serde::{de::Error, Deserialize};
use sp_core::{
	blake2_256, bytes,
	ed25519::{self, Public as EdPublic, Signature},
	Pair,
};
use tokio::sync::mpsc::{unbounded_channel, Sender};
use tracing::{error, info, trace};

use crate::{rpc, utils};

pub async fn finalized_headers(
	rpc_client: Client,
	message_tx: Sender<(Header, Instant)>,
	error_sender: Sender<anyhow::Error>,
) {
	if let Err(error) = subscribe_check_and_process(rpc_client, message_tx).await {
		error!("{error}");
		if let Err(error) = error_sender.send(error).await {
			error!("Cannot send error to error channel: {error}");
		}
	}
}

#[derive(Debug, Encode)]
enum SignerMessage {
	_DummyMessage(u32),
	PrecommitMessage(Precommit),
}

#[derive(Clone, Debug, Decode, Encode, Deserialize)]
struct Precommit {
	pub target_hash: H256,
	/// The target block's number
	pub target_number: u32,
}

#[derive(Clone, Debug, Decode, Deserialize)]
struct SignedPrecommit {
	pub precommit: Precommit,
	/// The signature on the message.
	pub signature: Signature,
	/// The Id of the signer.
	pub id: EdPublic,
}
#[derive(Clone, Debug, Decode, Deserialize)]
struct Commit {
	pub target_hash: H256,
	/// The target block's number.
	pub target_number: u32,
	/// Precommits for target block or any block after it that justify this commit.
	pub precommits: Vec<SignedPrecommit>,
}

#[derive(Clone, Debug, Decode)]
struct GrandpaJustification {
	pub round: u64,
	pub commit: Commit,
	pub _votes_ancestries: Vec<Header>,
}

impl<'de> Deserialize<'de> for GrandpaJustification {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let encoded = bytes::deserialize(deserializer)?;
		Self::decode(&mut &encoded[..])
			.map_err(|codec_err| D::Error::custom(format!("Invalid decoding: {:?}", codec_err)))
	}
}

#[derive(Clone, Debug)]
enum Messages {
	Justification(GrandpaJustification),
	ValidatorSetChange((Vec<EdPublic>, u64)),
	NewHeader(Header, Instant),
}

// Subscribes to finalized headers, justifications and monitors the changes in validator set.
// Verifies the justifications. Then sends the header off to be processed by LC.
async fn subscribe_check_and_process(
	subxt_client: Client,
	message_tx: Sender<(Header, Instant)>,
) -> Result<()> {
	let mut header_subscription = subxt_client
		.rpc()
		.subscribe_finalized_block_headers()
		.await?;
	// Get the hash of the head (finalized)
	let last_finalized_block_hash = rpc::get_chain_head_hash(&subxt_client).await?;

	// Current set of authorities, implicitly trusted, fetched from grandpa runtime.
	let mut validator_set =
		rpc::get_valset_by_hash(&subxt_client, last_finalized_block_hash).await?;

	// Fetch the set ID from storage at current height
	let mut set_id = rpc::get_set_id_by_hash(&subxt_client, last_finalized_block_hash).await?;

	// Get last (implicitly trusted) finalized block number
	let mut last_finalized_block_header =
		rpc::get_header_by_hash(&subxt_client, last_finalized_block_hash).await?;

	info!("Current set: {:?}", (validator_set.clone(), set_id));

	// Forming a channel for sending any relevant events gathered asynchronously through Substrate WS API.
	let (msg_sender, mut msg_receiver) = unbounded_channel::<Messages>();

	// Task that produces headers and new validator sets
	tokio::spawn({
		let msg_sender = msg_sender.clone();
		async move {
			while let Some(Ok(header)) = header_subscription.next().await {
				let received_at = Instant::now();
				msg_sender
					.send(Messages::NewHeader(header.clone(), received_at))
					.expect("Receiver should not be dropped.");

				// Search the header logs for validator set change
				let mut new_auths = utils::filter_auth_set_changes(header);

				// If the event exists, send the new auths over the message channel.
				if !new_auths.is_empty() {
					assert!(
						new_auths.len() == 1,
						"There should be only one valset change!"
					);
					let auths: Vec<(AuthorityId, u64)> = new_auths.pop().unwrap();
					let new_valset = auths
						.into_iter()
						.map(|(a, _)| EdPublic::from_raw(a.0 .0 .0))
						.collect();

					// Increment set_id
					set_id += 1;
					// Send it.
					msg_sender
						.send(Messages::ValidatorSetChange((new_valset, set_id)))
						.expect("Receiver should not be dropped.");
				}
			}
		}
	});

	// Subscribe to justifications.
	let j: Result<avail_subxt::rpc::Subscription<GrandpaJustification>, _> = subxt_client
		.rpc()
		.subscribe(
			"grandpa_subscribeJustifications",
			rpc_params![],
			"grandpa_unsubscribeJustifications",
		)
		.await;
	let mut justification_subscription = j?;

	// Task that produces justifications concurrently and just passes the justification to the main task.
	tokio::spawn(async move {
		while let Some(Ok(justification)) = justification_subscription.next().await {
			msg_sender
				.send(Messages::Justification(justification))
				.expect("Receiver should not be dropped.");
		}
	});

	// An accumulated collection of unverified headers and justifications that are matched one by one as headers/justifications arrive.
	let mut unverified_headers: Vec<(Header, Instant)> = vec![];
	let mut justifications: Vec<GrandpaJustification> = vec![];

	// Main loop, gathers blocks, justifications and validator sets and checks finality
	let res: Result<()> = 'mainloop: loop {
		let subxt_client = subxt_client.clone();
		match msg_receiver
			.recv()
			.await
			.ok_or(anyhow!("All senders dropped!"))?
		{
			Messages::Justification(justification) => {
				info!(
					"New justification at block no.: {}, hash: {:?}",
					justification.commit.target_number, justification.commit.target_hash
				);
				justifications.push(justification);
			},
			Messages::ValidatorSetChange(valset) => {
				info!("New validator set: {valset:?}");
				(validator_set, set_id) = valset;
			},
			Messages::NewHeader(header, received_at) => {
				info!("Header no.: {}", header.number);
				unverified_headers.push((header, received_at));
			},
		}

		while let Some((header, received_at)) = unverified_headers.pop() {
			let hash = Encode::using_encoded(&header, blake2_256).into();

			// Iterate through justifications and try to find a matching one.
			if let Some(pos) = justifications
				.iter()
				.position(|e| e.commit.target_hash == hash)
			{
				// Basically, pop it out of the collection.
				let justification = justifications.swap_remove(pos);
				// Form a message which is signed in the justification, it's a triplet of a Precommit, round number and set_id (taken from Substrate code).
				let signed_message = Encode::encode(&(
					&SignerMessage::PrecommitMessage(
						justification.commit.precommits[0].clone().precommit,
					),
					&justification.round,
					&set_id, // Set ID is needed here.
				));

				// Verify all the signatures of the justification signs the hash of the block and extract all the signer addreses.
				let signer_addresses = justification
					.commit
					.precommits
					.iter()
					.map(|precommit| {
						let is_ok = <ed25519::Pair as Pair>::verify_weak(
							&precommit.clone().signature.0[..],
							signed_message.as_slice(),
							precommit.clone().id,
						);
						is_ok
							.then(|| precommit.clone().id)
							.ok_or(anyhow!("Not signed by this signature!"))
					})
					.collect::<Result<Vec<_>>>();
				let Ok(signer_addresses) = signer_addresses else {
					break 'mainloop Err(signer_addresses.unwrap_err());
				};

				// Match all the signer addresses to the current validator set.
				let num_matched_addresses = signer_addresses
					.iter()
					.filter(|x| validator_set.iter().any(|e| e.0.eq(&x.0)))
					.count();

				info!(
					"Number of matching signatures: {num_matched_addresses}/{}",
					validator_set.len()
				);
				if num_matched_addresses < (validator_set.len() * 2 / 3) {
					break 'mainloop Err(anyhow!(
						"Not signed by the supermajority of the validator set."
					));
				}

				// Get all the skipped blocks, if they exist
				for bl_num in (last_finalized_block_header.number + 1)..header.number {
					info!("Sending skipped block {bl_num}");
					let header = rpc::get_header_by_block_number(&subxt_client, bl_num)
						.await?
						.0;
					message_tx.send((header, Instant::now())).await?;
				}

				info!("Sending finalized block {}", header.number);
				// Reset last finalized block
				last_finalized_block_header = header.clone();

				// Finally, send the verified block (header)
				message_tx.send((header, received_at)).await?;
			} else {
				trace!("Matched pair of header/justification not found.");
			}
		}
	};
	res
}
