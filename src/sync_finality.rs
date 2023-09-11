use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use avail_subxt::{api::runtime_types::sp_core::crypto::KeyTypeId, primitives::Header};
use codec::{Decode, Encode};
use futures::prelude::*;
use futures::stream::FuturesUnordered;
use rocksdb::DB;
use serde::de;
use serde::Deserialize;
use sp_core::{blake2_256, bytes::from_hex, ed25519, Pair, H256};
// use subxt::rpc_params;
use tokio::sync::mpsc::Sender;
use tracing::{error, info, trace};

use crate::{
	data::{get_finality_sync_checkpoint, store_finality_sync_checkpoint},
	rpc::RpcClient,
	types::{FinalitySyncCheckpoint, GrandpaJustification, SignerMessage, State},
	utils::filter_auth_set_changes,
};

#[derive(Debug, Deserialize, Clone)]
pub struct WrappedJustification(GrandpaJustification);

impl Decode for WrappedJustification {
	fn decode<I: codec::Input>(input: &mut I) -> std::result::Result<Self, codec::Error> {
		let j: Vec<u8> = Decode::decode(input)?;
		let jj: GrandpaJustification = Decode::decode(&mut &j[..])?;
		Ok(WrappedJustification(jj))
	}
}

#[derive(Debug, Deserialize, Decode, Clone)]
pub struct FinalityProof {
	/// The hash of block F for which justification is provided.
	pub block: H256,
	/// Justification of the block F.
	pub justification: WrappedJustification,
	/// The set of headers in the range (B; F] that we believe are unknown to the caller. Ordered.
	pub unknown_headers: Vec<Header>,
}

#[derive(Debug, Decode, Clone)]
pub struct WrappedProof(FinalityProof);

impl<'de> Deserialize<'de> for WrappedProof {
	fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let data = from_hex(&String::deserialize(deserializer)?)
			.map_err(|e| de::Error::custom(format!("{:?}", e)))?;
		Decode::decode(&mut &data[..]).map_err(|e| de::Error::custom(format!("{:?}", e)))
	}
}

pub trait SyncFinality {
	fn get_client(&self) -> RpcClient;
	fn get_db(&self) -> Arc<DB>;
}

pub struct SyncFinalityImpl {
	db: Arc<DB>,
	rpc_client: RpcClient,
}

impl SyncFinality for SyncFinalityImpl {
	fn get_client(&self) -> RpcClient {
		self.rpc_client.clone()
	}

	fn get_db(&self) -> Arc<DB> {
		self.db.clone()
	}
}

pub fn new(db: Arc<DB>, rpc_client: RpcClient) -> impl SyncFinality {
	SyncFinalityImpl { db, rpc_client }
}

async fn get_valset_at_genesis(
	rpc_client: RpcClient,
	genesis_hash: H256,
) -> Result<Vec<ed25519::Public>> {
	// Get validator set from genesis (Substrate Account ID)
	let validator_set_pre = rpc_client
		.get_session_valset_by_hash(genesis_hash)
		.await?
		.ok_or_else(|| anyhow!("Validator set is empty!"))?;

	// Get all grandpa session keys from genesis (GRANDPA ed25519 keys)
	let grandpa_keys_and_account = rpc_client
		.all_grandpa_session_keys_since(genesis_hash)
		.await?
		.map(|e| {
			rpc_client
				.get_session_key_owner(
					KeyTypeId(sp_core::crypto::key_types::GRANDPA.0),
					e,
					genesis_hash,
				)
				.map_ok(move |maybe_k| maybe_k.map(|k| (e, k)))
		})
		.collect::<FuturesUnordered<_>>()
		.try_collect::<Vec<_>>()
		.await?
		.into_iter()
		.collect::<Option<Vec<_>>>()
		.expect("Result is empty (grandpa key has no owner?)");

	// Filter just the keys that are found in the initial validator set
	let validator_set = grandpa_keys_and_account
		.into_iter()
		.filter(|(_, parent_acc)| validator_set_pre.iter().any(|e| e.0 == parent_acc.0))
		.map(|(grandpa_key, _)| grandpa_key)
		.collect::<Vec<_>>();
	Ok(validator_set)
}

pub async fn run(
	sync_finality_impl: impl SyncFinality,
	error_sender: Sender<anyhow::Error>,
	state: Arc<Mutex<State>>,
) {
	if let Err(err) = sync_finality(sync_finality_impl, state).await {
		error!("Cannot sync finality {err}");
		if let Err(error) = error_sender.send(err).await {
			error!("Cannot send error message: {error}");
		}
	};
}

pub async fn sync_finality(
	sync_finality: impl SyncFinality,
	state: Arc<Mutex<State>>,
) -> Result<()> {
	let rpc_client = sync_finality.get_client();
	let gen_hash = rpc_client.current_node().await.genesis_hash;

	let checkpoint = get_finality_sync_checkpoint(sync_finality.get_db())?;

	info!("Starting finality validation sync.");
	let mut set_id: u64;
	let mut curr_block_num = 1u32;
	let mut validator_set: Vec<ed25519::Public>;
	if let Some(ch) = checkpoint {
		info!("Continuing from block no {}", ch.number);
		set_id = ch.set_id;
		validator_set = ch.validator_set;
		curr_block_num = ch.number;
	} else {
		info!("No checkpoint found, starting from genesis.");
		validator_set = get_valset_at_genesis(rpc_client.clone(), gen_hash).await?;
		// Get set_id from genesis. Should be 0.
		set_id = rpc_client
			.get_grandpa_set_id(gen_hash)
			.await?
			.context("Set_id is empty?")?;
		info!("Set ID at genesis is {set_id}");
	}

	let mut header = rpc_client
		.get_chain_head_header()
		.await
		.context("Couldn't get finalized head!")?;
	let last_block_num = header.number;

	info!("Syncing finality from {curr_block_num} up to block no. {last_block_num}");

	let mut prev_hash = rpc_client.get_block_hash(curr_block_num - 1).await?;
	loop {
		if curr_block_num == last_block_num + 1 {
			info!("Finished verifying finality up to block no. {last_block_num}!");
			break;
		}
		header = rpc_client
			.get_header_by_block_number(curr_block_num)
			.await
			.context(format!("Couldn't get header for {curr_block_num}"))?
			.0;
		assert_eq!(header.parent_hash, prev_hash, "Parent hash doesn't match!");
		prev_hash = header.using_encoded(blake2_256).into();

		let next_validator_set = filter_auth_set_changes(&header);
		if next_validator_set.is_empty() {
			curr_block_num += 1;
			continue;
		}

		let proof = rpc_client
			.get_grandpa_finality_proof(curr_block_num)
			.await?;
		let proof_block_hash = proof.0.block;
		let p_h = rpc_client
			.get_header_by_hash(proof_block_hash)
			.await
			.context(format!("Couldn't get header for {}", proof_block_hash))?;

		let signed_message = Encode::encode(&(
			&SignerMessage::PrecommitMessage(
				proof.0.justification.0.commit.precommits[0]
					.clone()
					.precommit,
			),
			&proof.0.justification.0.round,
			&set_id,
		));
		// Verify all the signatures of the justification signs the hash of the block
		let signer_addresses = proof
			.0
			.justification
			.0
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

		let Ok(signer_addresses) = signer_addresses else {
			error!("Verification failed!");
			bail!("Some of the signatures don't match");
		};

		let num_matched_addresses = signer_addresses
			.iter()
			.filter(|x| validator_set.iter().any(|e| e.0.eq(&x.0)))
			.count();
		info!(
			"Number of matching signatures for block {curr_block_num}: {num_matched_addresses}/{}",
			validator_set.len()
		);
		if num_matched_addresses < (validator_set.len() * 2 / 3) {
			bail!("Not signed by the supermajority of the validator set.");
		} else {
			trace!("Proof in block: {}", p_h.number);
			curr_block_num += 1;

			validator_set = next_validator_set[0]
				.iter()
				.map(|a| ed25519::Public::from_raw(a.0 .0 .0 .0))
				.collect();
			set_id += 1;
			store_finality_sync_checkpoint(
				sync_finality.get_db(),
				FinalitySyncCheckpoint {
					number: curr_block_num,
					set_id,
					validator_set: validator_set.clone(),
				},
			)?;
		}
	}
	let mut state = state.lock().unwrap();
	state.set_finality_synced(true);
	info!("Finality is fully synced.");
	Ok(())
}
