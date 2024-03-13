use async_trait::async_trait;
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
use std::{
	iter::zip,
	sync::{Arc, Mutex},
};
use subxt::{storage::StorageKey, utils::AccountId32};
use tracing::{error, info, trace};

use crate::{
	data::{Database, FinalitySyncCheckpoint, Key},
	finality::{check_finality, ValidatorSet},
	network::rpc::{self, WrappedProof},
	shutdown::Controller,
	types::State,
	utils::filter_auth_set_changes,
};

#[async_trait]
pub trait Client {
	fn store_block_header(&self, block_number: u32, header: Header) -> Result<()>;
	fn get_checkpoint(&self) -> Result<Option<FinalitySyncCheckpoint>>;
	fn store_checkpoint(&self, checkpoint: FinalitySyncCheckpoint) -> Result<()>;
	async fn get_paged_storage_keys(
		&self,
		key: Vec<u8>,
		count: u32,
		start_key: Option<Vec<u8>>,
		hash: Option<H256>,
	) -> Result<Vec<StorageKey>>;
	async fn get_genesis_hash(&self) -> Result<H256>;
	async fn fetch_set_id_at(&self, block_hash: H256) -> Result<u64>;
	async fn get_validator_set_at(&self, block_hash: H256) -> Result<Option<Vec<AccountId32>>>;
	async fn get_session_key_owner_at(
		&self,
		block_hash: H256,
		public_key: ed25519::Public,
	) -> Result<Option<AccountId32>>;
	async fn get_block_hash(&self, block_number: u32) -> Result<H256>;
	async fn get_header_by_hash(&self, block_hash: H256) -> Result<Header>;
	async fn request_finality_proof(&self, block_number: u32) -> Result<WrappedProof>;
}

pub struct SyncFinality<T: Database + Sync> {
	db: T,
	rpc_client: rpc::Client,
}

impl<T: Database + Sync> SyncFinality<T> {
	pub fn new(db: T, rpc_client: rpc::Client) -> Self {
		SyncFinality { db, rpc_client }
	}
}

#[async_trait]
impl<T: Database + Sync> Client for SyncFinality<T> {
	async fn get_paged_storage_keys(
		&self,
		key: Vec<u8>,
		count: u32,
		start_key: Option<Vec<u8>>,
		hash: Option<H256>,
	) -> Result<Vec<StorageKey>> {
		self.rpc_client
			.get_paged_storage_keys(key, count, start_key, hash)
			.await
			.wrap_err("Finality Sync Client failed to get paged Storage Keys")
	}

	async fn get_genesis_hash(&self) -> Result<H256> {
		self.rpc_client
			.get_genesis_hash()
			.await
			.wrap_err("Finality Sync Client failed to get Genesis Hash")
	}

	async fn fetch_set_id_at(&self, block_hash: H256) -> Result<u64> {
		self.rpc_client
			.fetch_set_id_at(block_hash)
			.await
			.wrap_err("Finality Sync Client failed to fetch Set ID at provided Block Hash")
	}

	async fn get_validator_set_at(&self, block_hash: H256) -> Result<Option<Vec<AccountId32>>> {
		self.rpc_client
			.get_validator_set_at(block_hash)
			.await
			.wrap_err("Finality Sync Client failed to get Validator Set at provided Block Hash")
	}

	async fn get_session_key_owner_at(
		&self,
		block_hash: H256,
		public_key: ed25519::Public,
	) -> Result<Option<AccountId32>> {
		self.rpc_client
			.get_session_key_owner_at(block_hash, public_key)
			.await
			.wrap_err(
				"Finality Sync Client failed to get Session Key Owner for provided Block Hash",
			)
	}

	async fn get_block_hash(&self, block_number: u32) -> Result<H256> {
		self.rpc_client
			.get_block_hash(block_number)
			.await
			.wrap_err("Finality Sync Client failed to get Block Hash")
	}

	async fn get_header_by_hash(&self, block_hash: H256) -> Result<Header> {
		self.rpc_client
			.get_header_by_hash(block_hash)
			.await
			.wrap_err("Finality Sync Client failed to get Header by Hash")
	}

	async fn request_finality_proof(&self, block_number: u32) -> Result<WrappedProof> {
		self.rpc_client
			.request_finality_proof(block_number)
			.await
			.wrap_err("Finality Sync Client failed to request Finality Proof")
	}

	fn store_block_header(&self, block_number: u32, header: Header) -> Result<()> {
		self.db
			.put(Key::BlockHeader(block_number), header)
			.wrap_err("Finality Sync Client failed to store Block Header")
	}

	fn get_checkpoint(&self) -> Result<Option<FinalitySyncCheckpoint>> {
		self.db
			.get(Key::FinalitySyncCheckpoint)
			.wrap_err("Finality Sync Client failed to get Checkpoint")
	}

	fn store_checkpoint(&self, checkpoint: FinalitySyncCheckpoint) -> Result<()> {
		self.db
			.put(Key::FinalitySyncCheckpoint, checkpoint)
			.wrap_err("Finality Sync Client failed to store Checkpoint")
	}
}

const GRANDPA_KEY_ID: [u8; 4] = *b"gran";
const GRANDPA_KEY_LEN: usize = 32;

async fn get_valset_at_genesis(
	client: &impl Client,
	genesis_hash: H256,
) -> Result<Vec<ed25519::Public>> {
	let mut k1 = twox_128("Session".as_bytes()).to_vec();
	let mut k2 = twox_128("KeyOwner".as_bytes()).to_vec();
	k1.append(&mut k2);

	let validator_set_pre = client
		.get_validator_set_at(genesis_hash)
		.await
		.wrap_err("Couldn't get initial validator set")?
		.ok_or_else(|| eyre!("Validator set is empty!"))?;

	// Get all grandpa session keys from genesis (GRANDPA ed25519 keys)
	let grandpa_keys = client
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
		.collect::<Vec<ed25519::Public>>();

	let grandpa_account_results = join_all(
		grandpa_keys
			.clone()
			.iter()
			.map(|&e| client.get_session_key_owner_at(genesis_hash, e)),
	)
	.await;
	let grandpa_accounts = grandpa_account_results.into_iter().map(|a| {
		a.expect("Couldn't get session key owner for grandpa key!")
			.expect("Result is empty (grandpa key has no owner?)")
	});

	let grandpa_keys_and_account = zip(grandpa_keys, grandpa_accounts);

	// Filter just the keys that are found in the initial validator set
	let validator_set = grandpa_keys_and_account
		.into_iter()
		.filter(|(_, parent_acc)| validator_set_pre.iter().any(|e| e.0 == parent_acc.0))
		.map(|(grandpa_key, _)| grandpa_key)
		.collect::<Vec<_>>();
	Ok(validator_set)
}

pub async fn run(
	client: impl Client,
	shutdown: Controller<String>,
	state: Arc<Mutex<State>>,
	from_header: Header,
) {
	if let Err(error) = sync(client, state, from_header).await {
		error!("Cannot sync finality {error}");
		let _ = shutdown.trigger_shutdown(format!("Cannot sync finality {error:#}"));
	};
}

pub async fn sync(
	client: impl Client,
	state: Arc<Mutex<State>>,
	mut from_header: Header,
) -> Result<()> {
	let gen_hash = client.get_genesis_hash().await?;

	let checkpoint = client.get_checkpoint()?;

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
		validator_set = get_valset_at_genesis(&client, gen_hash).await?;
		// get set_id from genesis. Should be 0.
		set_id = client
			.fetch_set_id_at(gen_hash)
			.await
			.wrap_err(format!("Couldn't get set_id at {}", gen_hash))?;
		info!("Set ID at genesis is {set_id}");
	}

	let last_block_num = from_header.number;

	info!("Syncing finality from {curr_block_num} up to block no. {last_block_num}");

	let mut prev_hash = client
		.get_block_hash(curr_block_num - 1)
		.await
		.wrap_err("Hash doesn't exist?")?;
	loop {
		if curr_block_num == last_block_num + 1 {
			info!("Finished verifying finality up to block no. {last_block_num}!");
			break;
		}
		let hash = client
			.get_block_hash(curr_block_num)
			.await
			.wrap_err(format!(
				"Couldn't get hash for block no. {}",
				curr_block_num
			))?;
		from_header = client
			.get_header_by_hash(hash)
			.await
			.wrap_err(format!("Couldn't get header for {}", hash))?;
		client.store_block_header(curr_block_num, from_header.clone())?;

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

		let proof: WrappedProof = client
			.request_finality_proof(curr_block_num)
			.await
			.wrap_err(format!(
				"Couldn't get finality proof for block no. {}",
				curr_block_num
			))?;
		let proof_block_hash = proof.0.block;
		let p_h = client
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
		client.store_checkpoint(FinalitySyncCheckpoint {
			number: curr_block_num,
			set_id,
			validator_set: validator_set.clone(),
		})?;
	}
	state.lock().unwrap().finality_synced = true;
	info!("Finality is fully synced.");
	Ok(())
}
