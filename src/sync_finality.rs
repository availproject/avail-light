use avail_subxt::primitives::Header;
use codec::Encode;
use color_eyre::{
	eyre::{eyre, Context},
	Result,
};
use futures::future::join_all;
use sp_core::{
	blake2_256,
	ed25519::{self},
	twox_128, H256,
};
use std::sync::{Arc, Mutex};
use tracing::{error, info, trace};

use crate::{
	data::{FinalitySyncCheckpoint, Key},
	db::data::DB,
	finality::{check_finality, ValidatorSet},
	network::rpc::{self, WrappedProof},
	shutdown::Controller,
	types::State,
	utils::filter_auth_set_changes,
};

pub struct SyncFinality<T: DB> {
	db: T,
	rpc_client: rpc::Client,
}

impl<T: DB> SyncFinality<T> {
	fn get_client(&self) -> rpc::Client {
		self.rpc_client.clone()
	}

	fn store_block_header(&self, block_number: u32, header: Header) -> Result<()> {
		self.db
			.put(Key::BlockHeader(block_number), header)
			.wrap_err("Finality Sync failed to store Block Header")
	}

	fn get_checkpoint(&self) -> Result<Option<FinalitySyncCheckpoint>> {
		self.db
			.get(Key::FinalitySyncCheckpoint)
			.wrap_err("Finality Sync failed to get Checkpoint")
	}

	fn store_checkpoint(&self, checkpoint: FinalitySyncCheckpoint) -> Result<()> {
		self.db
			.put(Key::FinalitySyncCheckpoint, checkpoint)
			.wrap_err("Finality Sync failed to store Checkpoint")
	}
}

pub fn new<T: DB>(db: T, rpc_client: rpc::Client) -> SyncFinality<T> {
	SyncFinality { db, rpc_client }
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

pub async fn run<T: DB>(
	sync_finality: SyncFinality<T>,
	shutdown: Controller<String>,
	state: Arc<Mutex<State>>,
	from_header: Header,
) {
	if let Err(error) = sync(sync_finality, state, from_header).await {
		error!("Cannot sync finality {error}");
		let _ = shutdown.trigger_shutdown(format!("Cannot sync finality {error:#}"));
	};
}

pub async fn sync<T: DB>(
	sync_finality: SyncFinality<T>,
	state: Arc<Mutex<State>>,
	mut from_header: Header,
) -> Result<()> {
	let rpc_client = sync_finality.get_client();
	let gen_hash = rpc_client.get_genesis_hash().await?;

	let checkpoint = sync_finality.get_checkpoint()?;

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
		sync_finality.store_block_header(curr_block_num, from_header.clone())?;

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

		let valset = ValidatorSet {
			set_id,
			validator_set,
		};
		check_finality(&valset, &proof.0.justification.0).context("Finality sync check failed")?;

		trace!("Proof in block: {}", p_h.number);
		curr_block_num += 1;

		validator_set = next_validator_set[0]
			.iter()
			.map(|a| ed25519::Public::from_raw(a.0 .0 .0 .0))
			.collect();
		set_id += 1;
		sync_finality.store_checkpoint(FinalitySyncCheckpoint {
			number: curr_block_num,
			set_id,
			validator_set: validator_set.clone(),
		})?;
	}
	state.lock().unwrap().finality_synced = true;
	info!("Finality is fully synced.");
	Ok(())
}
