use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use avail_subxt::{
	api::{self, runtime_types::sp_core::crypto::KeyTypeId},
	avail::{self, Client},
	primitives::Header,
	rpc::rpc_params,
};
use codec::{Decode, Encode};
use futures::future::join_all;
use rocksdb::DB;
use serde::de::{self};
use serde::Deserialize;
use sp_core::{
	blake2_256,
	bytes::from_hex,
	ed25519::{self},
	twox_128, Pair, H256,
};
// use subxt::rpc_params;
use tokio::sync::mpsc::Sender;
use tracing::{error, info, trace};

use crate::{
	data::{get_finality_sync_checkpoint, store_finality_sync_checkpoint},
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
	fn get_client(&self) -> avail::Client;
	fn get_db(&self) -> Arc<DB>;
}

pub struct SyncFinalityImpl {
	db: Arc<DB>,
	rpc_client: avail::Client,
}

impl SyncFinality for SyncFinalityImpl {
	fn get_client(&self) -> avail::Client {
		self.rpc_client.clone()
	}

	fn get_db(&self) -> Arc<DB> {
		self.db.clone()
	}
}

pub fn new(db: Arc<DB>, rpc_client: avail::Client) -> impl SyncFinality {
	SyncFinalityImpl { db, rpc_client }
}

const GRANDPA_KEY_ID: [u8; 4] = *b"gran";
const GRANDPA_KEY_LEN: usize = 32;

async fn get_valset_at_genesis(
	rpc_client: Client,
	genesis_hash: H256,
) -> Result<Vec<ed25519::Public>> {
	let mut k1 = twox_128("Session".as_bytes()).to_vec();
	let mut k2 = twox_128("KeyOwner".as_bytes()).to_vec();
	k1.append(&mut k2);

	// Get validator set from genesis (Substrate Account ID)
	let validators_key = api::storage().session().validators();
	let validator_set_pre = rpc_client
		.clone()
		.storage()
		.at(genesis_hash)
		.fetch(&validators_key)
		.await
		.context("Couldn't get initial validator set")?
		.ok_or_else(|| anyhow!("Validator set is empty!"))?;

	// Get all grandpa session keys from genesis (GRANDPA ed25519 keys)
	let grandpa_keys_and_account = rpc_client
		.rpc()
		// Get all storage keys that correspond to Session_KeyOwner query, then filter by "gran"
		// Perhaps there is a better way, but I don't know it
		.storage_keys_paged(&k1, 1000, None, Some(genesis_hash))
		.await
		.context("Couldn't get storage keys associated with key owners!")?
		.into_iter()
		// throw away the beginning, we don't need it
		.map(|e| e.0[k1.len()..].to_vec())
		.filter(|e| {
			// exclude the actual key (at the end of the storage key) from search, we may find "gran" by accident
			e[..(e.len() - GRANDPA_KEY_LEN)]
				.windows(GRANDPA_KEY_ID.len())
				.any(|e| e == GRANDPA_KEY_ID)
		})
		.map(|e| e.as_slice()[e.len() - GRANDPA_KEY_LEN..].to_vec())
		.map(|e| ed25519::Public::from_raw(e.try_into().expect("Vector isn't 32 bytes long")))
		.map(|e| {
			let c = rpc_client.clone();
			async move {
				let session_key_key_owner = api::storage()
					.session()
					.key_owner(KeyTypeId(sp_core::crypto::key_types::GRANDPA.0), e.0);
				let k = c
					.storage()
					.at(genesis_hash)
					.fetch(&session_key_key_owner)
					.await
					.expect("Couldn't get session key owner for grandpa key!")
					.expect("Result is empty (grandpa key has no owner?)");
				(e, k)
			}
		});

	let grandpa_keys_and_account = join_all(grandpa_keys_and_account).await;

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
	let gen_hash = rpc_client.genesis_hash();

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
		let set_id_key = api::storage().grandpa().current_set_id();
		set_id = rpc_client
			.storage()
			.at(gen_hash)
			.fetch(&set_id_key)
			.await
			.context(format!("Couldn't get set_id at {}", gen_hash))?
			.context("Set_id is empty?")?;
		info!("Set ID at genesis is {set_id}");
	}

	let head = rpc_client
		.rpc()
		.finalized_head()
		.await
		.context("Couldn't get finalized head!")?;
	let mut header = rpc_client
		.rpc()
		.header(Some(head))
		.await
		.context("Couldn't get finalized head header!")?
		.context("Finalized head header not found!")?;
	let last_block_num = header.number;

	info!("Syncing finality from {curr_block_num} up to block no. {last_block_num}");

	let mut prev_hash = rpc_client
		.rpc()
		.block_hash(Some((curr_block_num - 1).into()))
		.await?
		.context("Hash doesn't exist?")?;
	loop {
		if curr_block_num == last_block_num + 1 {
			info!("Finished verifying finality up to block no. {last_block_num}!");
			break;
		}
		let hash = rpc_client
			.rpc()
			.block_hash(Some(curr_block_num.into()))
			.await
			.context(format!(
				"Couldn't get hash for block no. {}",
				curr_block_num
			))?
			.context(format!("Hash for block no. {} not found!", curr_block_num))?;
		header = rpc_client
			.rpc()
			.header(Some(hash))
			.await
			.context(format!("Couldn't get header for {}", hash))?
			.context(format!("Header for hash {} not found!", hash))?;
		assert_eq!(header.parent_hash, prev_hash, "Parent hash doesn't match!");
		prev_hash = header.using_encoded(blake2_256).into();

		let next_validator_set = filter_auth_set_changes(&header);
		if next_validator_set.is_empty() {
			curr_block_num += 1;
			continue;
		}

		let proof: WrappedProof = rpc_client
			.rpc()
			.request("grandpa_proveFinality", rpc_params![curr_block_num])
			.await
			.context(format!(
				"Couldn't get finality proof for block no. {}",
				curr_block_num
			))?;
		let proof_block_hash = proof.0.block;
		let p_h = rpc_client
			.rpc()
			.header(Some(proof_block_hash))
			.await
			.context(format!("Couldn't get header for {}", proof_block_hash))?
			.context(format!("Header for hash {} not found!", proof_block_hash))?;

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
