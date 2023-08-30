//! Persistence to RocksDB.

use anyhow::{anyhow, Context, Result};
use avail_core::AppId;
use avail_subxt::primitives::Header as DaHeader;
use avail_subxt::utils::H256;
use codec::{Decode, Encode};
use rocksdb::{ColumnFamilyDescriptor, Options, DB};
use std::sync::Arc;

use crate::{
	consts::{APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF, STATE_CF},
	types::FinalitySyncCheckpoint,
};

#[derive(Clone)]
pub struct Db(Arc<DB>);

impl Db {
	const LAST_FULL_NODE_WS_KEY: &str = "last_full_node_ws";
	const GENESIS_HASH_KEY: &str = "genesis_hash";
	const FINALITY_SYNC_CHECKPOINT_KEY: &str = "finality_sync_checkpoint";

	pub fn new(path: impl AsRef<std::path::Path>) -> Result<Self> {
		let mut confidence_cf_opts = Options::default();
		confidence_cf_opts.set_max_write_buffer_number(16);

		let mut block_header_cf_opts = Options::default();
		block_header_cf_opts.set_max_write_buffer_number(16);

		let mut app_data_cf_opts = Options::default();
		app_data_cf_opts.set_max_write_buffer_number(16);

		let mut state_cf_opts = Options::default();
		state_cf_opts.set_max_write_buffer_number(16);

		let cf_opts = vec![
			ColumnFamilyDescriptor::new(CONFIDENCE_FACTOR_CF, confidence_cf_opts),
			ColumnFamilyDescriptor::new(BLOCK_HEADER_CF, block_header_cf_opts),
			ColumnFamilyDescriptor::new(APP_DATA_CF, app_data_cf_opts),
			ColumnFamilyDescriptor::new(STATE_CF, state_cf_opts),
		];

		let mut db_opts = Options::default();
		db_opts.create_if_missing(true);
		db_opts.create_missing_column_families(true);

		let db = DB::open_cf_descriptors(&db_opts, path, cf_opts)?;

		Ok(Self(Arc::new(db)))
	}

	fn get(&self, cf_name: &str, key: impl AsRef<[u8]>) -> Result<Option<Vec<u8>>> {
		let cf = self
			.0
			.cf_handle(cf_name)
			.with_context(|| format!("Failed to get column `{cf_name}\" in DB"))?;
		let key = key.as_ref();
		self.0
			.get_cf(&cf, key)
			.with_context(|| format!("Failed to get key {key:?} in column `{cf_name}' in DB",))
	}

	fn get_pinned(
		&'_ self,
		cf_name: &'_ str,
		key: impl AsRef<[u8]>,
	) -> Result<Option<rocksdb::DBPinnableSlice<'_>>> {
		let cf = self
			.0
			.cf_handle(cf_name)
			.with_context(|| format!("Failed to get column `{cf_name}\" in DB"))?;
		let key = key.as_ref();
		self.0
			.get_pinned_cf(&cf, key)
			.with_context(|| format!("Failed to get key {key:?} in column `{cf_name}' in DB",))
	}

	fn put(&self, cf_name: &str, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Result<()> {
		let cf = self
			.0
			.cf_handle(cf_name)
			.with_context(|| format!("Failed to get column `{cf_name}\" in DB"))?;
		let key = key.as_ref();
		self.0
			.put_cf(&cf, key, value)
			.with_context(|| format!("Failed to write key {key:?} in column `{cf_name}' in DB",))
	}

	pub fn store_last_full_node_ws(&self, last_full_node_ws: &str) -> Result<()> {
		self.put(STATE_CF, Self::LAST_FULL_NODE_WS_KEY, last_full_node_ws)
			.context("Failed to write last full node ws")
	}

	pub fn get_last_full_node_ws(&self) -> Result<Option<String>> {
		self.get(STATE_CF, Self::LAST_FULL_NODE_WS_KEY)
			.context("Failed to get last full node ws")?
			.map(String::from_utf8)
			.transpose()
			.context("Invalid non utf8 full node ws")
	}

	pub fn store_app_data(&self, app_id: AppId, block_number: u32, data: &[u8]) -> Result<()> {
		self.put(APP_DATA_CF, format!("{}:{block_number}", app_id.0), data)
			.context("Failed to store app data")
	}

	pub fn get_app_data(&self, app_id: AppId, block_number: u32) -> Result<Option<Vec<u8>>> {
		self.get(APP_DATA_CF, format!("{}:{block_number}", app_id.0))
			.context("Failed to get app data")
	}

	/// Encodes and stores app data into database under the `app_id:block_number` key
	pub fn store_encoded_app_data(
		&self,
		app_id: AppId,
		block_number: u32,
		data: &impl Encode,
	) -> Result<()> {
		self.store_app_data(app_id, block_number, &data.encode())
	}

	/// Gets and decodes app data from database for the `app_id:block_number` key
	pub fn get_decoded_app_data<T: Decode>(
		&self,
		app_id: AppId,
		block_number: u32,
	) -> Result<Option<T>> {
		self.get_app_data(app_id, block_number)?
			.map(|bytes| T::decode(&mut &*bytes))
			.transpose()
			.context("Failed decoding the app data.")
	}

	/// Checks if block header for given block number is in database
	pub fn contains_block_header(&self, block_number: u32) -> Result<bool> {
		self.get_pinned(BLOCK_HEADER_CF, block_number.to_be_bytes())
			.context("Failed to get block header")
			.map(|value| value.is_some())
	}

	/// Stores block header into database under the given block number key
	pub fn store_block_header(&self, block_number: u32, header: &DaHeader) -> Result<()> {
		self.put(
			BLOCK_HEADER_CF,
			block_number.to_be_bytes(),
			serde_json::to_vec(header)?,
		)
		.context("Failed to write block header")
	}

	/// Checks if confidence factor for given block number is in database
	pub fn contains_confidence(&self, block_number: u32) -> Result<bool> {
		self.get_pinned(CONFIDENCE_FACTOR_CF, block_number.to_be_bytes())
			.context("Failed to get confidence")
			.map(|value| value.is_some())
	}

	/// Gets confidence factor from database for given block number
	pub fn get_confidence(&self, block_number: u32) -> Result<Option<u32>> {
		self.get(CONFIDENCE_FACTOR_CF, block_number.to_be_bytes())
			.context("Couldn't get confidence in db")?
			.map(|bytes| bytes.try_into().map(u32::from_be_bytes))
			.transpose()
			.map_err(|_| anyhow!("Conversion failed"))
			.context("Unable to convert confindence (wrong number of bytes)")
	}

	/// Stores confidence factor into database under the given block number key
	pub fn store_confidence(&self, block_number: u32, count: u32) -> Result<()> {
		self.put(
			CONFIDENCE_FACTOR_CF,
			block_number.to_be_bytes(),
			count.to_be_bytes(),
		)
		.context("Failed to write confidence")
	}

	pub fn get_genesis_hash(&self) -> Result<Option<H256>> {
		self.get(STATE_CF, Self::GENESIS_HASH_KEY)
			.context("Couldn't get genesis hash from db")?
			.map(|bytes| bytes.try_into().map(H256))
			.transpose()
			.map_err(|_| anyhow!("Bad genesis hash format!"))
	}

	pub fn store_genesis_hash(&self, genesis_hash: H256) -> Result<()> {
		self.put(STATE_CF, Self::GENESIS_HASH_KEY, genesis_hash)
			.context("Failed to write genesis hash to db")
	}

	pub fn get_finality_sync_checkpoint(&self) -> Result<Option<FinalitySyncCheckpoint>> {
		self.get(STATE_CF, Self::FINALITY_SYNC_CHECKPOINT_KEY)
			.context("Couldn't get finality sync checkpoint from db")?
			.map(|bytes| FinalitySyncCheckpoint::decode(&mut &*bytes))
			.transpose()
			.context("Failed to decoded finality sync checkpoint")
	}

	pub fn store_finality_sync_checkpoint(&self, checkpoint: FinalitySyncCheckpoint) -> Result<()> {
		self.put(
			STATE_CF,
			Self::FINALITY_SYNC_CHECKPOINT_KEY,
			checkpoint.encode(),
		)
		.context("Failed to write finality sync checkpoint data")
	}
}
