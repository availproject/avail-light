use avail_subxt::primitives::Header;
use codec::Encode;
use color_eyre::{
	eyre::{bail, eyre, Context},
	Report, Result,
};
use futures::future::join_all;
use rocksdb::DB;
use sp_core::{
	blake2_256,
	ed25519::{self},
	twox_128, Pair, H256,
};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::Sender;
use tracing::{error, info, trace};

use crate::{
	data::{
		get_finality_sync_checkpoint, store_block_header_in_db, store_finality_sync_checkpoint,
	},
	network::rpc::{self, WrappedProof},
	types::{FinalitySyncCheckpoint, SignerMessage, State},
	utils::filter_auth_set_changes,
};

pub trait SyncFinality {
	fn get_client(&self) -> rpc::Client;
	fn get_db(&self) -> Arc<DB>;
}

pub struct SyncFinalityImpl {
	db: Arc<DB>,
	rpc_client: rpc::Client,
}

impl SyncFinality for SyncFinalityImpl {
	fn get_client(&self) -> rpc::Client {
		self.rpc_client.clone()
	}

	fn get_db(&self) -> Arc<DB> {
		self.db.clone()
	}
}

pub fn new(db: Arc<DB>, rpc_client: rpc::Client) -> impl SyncFinality {
	SyncFinalityImpl { db, rpc_client }
}

const GRANDPA_KEY_ID: [u8; 4] = *b"gran";
const GRANDPA_KEY_LEN: usize = 32;

async fn get_valset_at_genesis(
	rpc_client: rpc::Client,
	genesis_hash: H256,
) -> Result<Vec<ed25519::Public>> {
	let mut k1 = twox_128("Session".as_bytes()).to_vec();
	let mut k2 = twox_128("KeyOwner".as_bytes()).to_vec();
	k1.append(&mut k2);

	let validator_set_pre = rpc_client
		.get_validator_set_at(genesis_hash)
		.await
		.wrap_err("Couldn't get initial validator set")?
		.ok_or_else(|| eyre!("Validator set is empty!"))?;

	// Get all grandpa session keys from genesis (GRANDPA ed25519 keys)
	let grandpa_keys_and_account = rpc_client
		// get all storage keys that correspond to Session_KeyOwner query, then filter by "gran"
		// perhaps there is a better way, but I don't know it
		.get_paged_storage_keys(k1.clone(), 1000, None, Some(genesis_hash))
		.await
		.wrap_err("Couldn't get storage keys associated with key owners!")?
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
		.map(|e: ed25519::Public| {
			let c = rpc_client.clone();
			async move {
				let k = c
					.get_session_key_owner_at(genesis_hash, e)
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
	error_sender: Sender<Report>,
	state: Arc<Mutex<State>>,
	from_header: Header,
) {
	if let Err(err) = sync_finality(sync_finality_impl, state, from_header).await {
		error!("Cannot sync finality {err}");
		if let Err(error) = error_sender.send(err).await {
			error!("Cannot send error message: {error}");
		}
	};
}

pub async fn sync_finality(
	sync_finality: impl SyncFinality,
	state: Arc<Mutex<State>>,
	mut from_header: Header,
) -> Result<()> {
	let rpc_client = sync_finality.get_client();
	let gen_hash = rpc_client.get_genesis_hash().await?;

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
		// get set_id from genesis. Should be 0.
		set_id = rpc_client
			.fetch_set_id_at(gen_hash)
			.await
			.wrap_err(format!("Couldn't get set_id at {}", gen_hash))?;
		info!("Set ID at genesis is {set_id}");
	}

	let last_block_num = from_header.number;

	info!("Syncing finality from {curr_block_num} up to block no. {last_block_num}");
	let db = sync_finality.get_db();

	let mut prev_hash = rpc_client
		.get_block_hash(curr_block_num - 1)
		.await
		.wrap_err("Hash doesn't exist?")?;
	loop {
		if curr_block_num == last_block_num + 1 {
			info!("Finished verifying finality up to block no. {last_block_num}!");
			break;
		}
		let hash = rpc_client
			.get_block_hash(curr_block_num)
			.await
			.wrap_err(format!(
				"Couldn't get hash for block no. {}",
				curr_block_num
			))?;
		from_header = rpc_client
			.get_header_by_hash(hash)
			.await
			.wrap_err(format!("Couldn't get header for {}", hash))?;
		store_block_header_in_db(db.clone(), curr_block_num, &from_header)?;

		assert_eq!(
			from_header.parent_hash, prev_hash,
			"Parent hash doesn't match!"
		);
		prev_hash = from_header.using_encoded(blake2_256).into();

		let next_validator_set = filter_auth_set_changes(&from_header);
		if next_validator_set.is_empty() {
			curr_block_num += 1;
			continue;
		}

		let proof: WrappedProof = rpc_client
			.request_finality_proof(curr_block_num)
			.await
			.wrap_err(format!(
				"Couldn't get finality proof for block no. {}",
				curr_block_num
			))?;
		let proof_block_hash = proof.0.block;
		let p_h = rpc_client
			.get_header_by_hash(proof_block_hash)
			.await
			.wrap_err(format!("Couldn't get header for {}", proof_block_hash))?;

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
					.ok_or_else(|| eyre!("Not signed by this signature!"))
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
	state.lock().unwrap().finality_synced = true;
	info!("Finality is fully synced.");
	Ok(())
}
