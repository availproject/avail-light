use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use avail_subxt::{
	api::{self, runtime_types::sp_core::crypto::KeyTypeId},
	avail,
	primitives::Header,
	rpc::rpc_params,
};
use codec::{Decode, Encode};
use futures_util::future::join_all;
use mockall::automock;
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
	types::{GrandpaJustification, SignerMessage},
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

#[async_trait]
#[automock]
pub trait SyncFinality {
	fn get_client(&self) -> avail::Client;
}

pub struct SyncFinalityImpl {
	_db: Arc<DB>,
	rpc_client: avail::Client,
}

impl SyncFinality for SyncFinalityImpl {
	fn get_client(&self) -> avail::Client {
		self.rpc_client.clone()
	}
}

pub fn new(db: Arc<DB>, rpc_client: avail::Client) -> impl SyncFinality {
	SyncFinalityImpl {
		_db: db,
		rpc_client,
	}
}

const GRANDPA_KEY_ID: [u8; 4] = *b"gran";
const GRANDPA_KEY_LEN: usize = 32;

pub async fn run(sync_finality: impl SyncFinality, error_sender: Sender<anyhow::Error>) {
	if let Err(err) = sync_finality_f(sync_finality).await {
		error!("Cannot sync finality {err}");
		if let Err(error) = error_sender.send(err).await {
			error!("Cannot send error message: {error}");
		}
		return;
	};
}

pub async fn sync_finality_f(sync_finality: impl SyncFinality) -> Result<()> {
	let rpc_client = sync_finality.get_client();
	let gen_hash = rpc_client.genesis_hash();

	let mut k1 = twox_128("Session".as_bytes()).to_vec();
	let mut k2 = twox_128("KeyOwner".as_bytes()).to_vec();
	k1.append(&mut k2);

	info!("Starting finality validation sync.");
	// Get validator set from genesis (Substrate Account ID)
	let validators_key = api::storage().session().validators();
	let validator_set_pre = rpc_client
		.clone()
		.storage()
		.at(gen_hash)
		.fetch(&validators_key)
		.await
		.context("Couldn't get initial validator set")?
		.ok_or(anyhow!("Validator set is empty!"))?;

	// Get all grandpa session keys from genesis (GRANDPA ed25519 keys)
	let grandpa_keys_and_account = rpc_client
		.rpc()
		// Get all storage keys that correspond to Session_KeyOwner query, then filter by "gran"
		// Perhaps there is a better way, but I don't know it
		.storage_keys_paged(&k1, 1000, None, Some(gen_hash))
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
					.at(gen_hash)
					.fetch(&session_key_key_owner)
					.await
					.expect("Couldn't get session key owner for grandpa key!")
					.expect("Result is empty (grandpa key has no owner?)");
				(e, k)
			}
		});

	let grandpa_keys_and_account = join_all(grandpa_keys_and_account).await;

	// Filter just the keys that are found in the initial validator set
	let mut validator_set = grandpa_keys_and_account
		.into_iter()
		.filter(|(_, parent_acc)| validator_set_pre.iter().any(|e| e.0 == parent_acc.0))
		.map(|(grandpa_key, _)| grandpa_key)
		.collect::<Vec<_>>();

	info!("Initial validator set at genesis is: {:?}", validator_set);

	// Get set_id from genesis. Should be 0.
	let set_id_key = api::storage().grandpa().current_set_id();
	let mut set_id = rpc_client
		.storage()
		.at(gen_hash)
		.fetch(&set_id_key)
		.await
		.context(format!("Couldn't get set_id at {}", gen_hash))?
		.context("Set_id is empty?")?;
	info!("Set ID at genesis is {set_id}");

	let head = rpc_client
		.rpc()
		.finalized_head()
		.await
		.context("Couldn't get finalized head!")?;
	let mut head = rpc_client
		.rpc()
		.header(Some(head))
		.await
		.context("Couldn't get finalized head header!")?
		.context("Finalized head header not found!")?;
	let last_block_num = head.number;

	info!("Syncing finality up to block no. {last_block_num}");
	let mut curr_block_num = 1u32;
	let mut prev_hash = gen_hash;
	loop {
		if curr_block_num == last_block_num + 1 {
			info!("Finished verifying finality up to block no. {last_block_num}!");
			break;
		}
		let h = rpc_client
			.rpc()
			.block_hash(Some(curr_block_num.into()))
			.await
			.context(format!(
				"Couldn't get hash for block no. {}",
				curr_block_num
			))?
			.context(format!("Hash for block no. {} not found!", curr_block_num))?;
		head = rpc_client
			.rpc()
			.header(Some(h))
			.await
			.context(format!("Couldn't get header for {}", h))?
			.context(format!("Header for hash {} not found!", h))?;
		assert_eq!(head.parent_hash, prev_hash, "Parent hash doesn't match!");
		prev_hash = head.using_encoded(blake2_256).into();

		let f = filter_auth_set_changes(&head);
		if f.is_empty() {
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
					.ok_or(anyhow!("Not signed by this signature!"))
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
			"Number of matching signatures: {num_matched_addresses}/{}",
			validator_set.len()
		);
		if num_matched_addresses < (validator_set.len() * 2 / 3) {
			bail!("Not signed by the supermajority of the validator set.");
		}

		trace!("Proof in block: {}", p_h.number);
		curr_block_num += 1;

		validator_set = f[0]
			.iter()
			.map(|a| ed25519::Public::from_raw(a.0 .0 .0 .0))
			.collect();
		set_id += 1;
	}
	Ok(())
}
