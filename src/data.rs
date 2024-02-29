//! Persistence to RocksDB.

use avail_core::AppId;
use avail_subxt::primitives::Header as DaHeader;
use codec::{Decode, Encode};
use color_eyre::{
	eyre::{eyre, WrapErr},
	Result,
};
use kate_recovery::com::AppData;
use rocksdb::{ColumnFamilyDescriptor, Options, DB};
use std::sync::Arc;

use crate::{
	consts::{APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF, STATE_CF},
	types::FinalitySyncCheckpoint,
};

const FINALITY_SYNC_CHECKPOINT_KEY: &str = "finality_sync_checkpoint";

/// Initializes Rocks Database
pub fn init_db(path: &str) -> Result<Arc<DB>> {
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
	Ok(Arc::new(db))
}

/// Gets the block header from database
pub fn get_block_header_from_db(db: Arc<DB>, block_number: u32) -> Result<Option<DaHeader>> {
	let handle = db
		.cf_handle(BLOCK_HEADER_CF)
		.ok_or_else(|| eyre!("Failed to get cf handle"))?;

	db.get_cf(&handle, block_number.to_be_bytes())
		.wrap_err("Failed to get block header")?
		.map(|value| serde_json::from_slice(&value).wrap_err("Failed to deserialize header"))
		.transpose()
}

/// Stores block header into database under the given block number key
pub fn store_block_header_in_db(db: Arc<DB>, block_number: u32, header: &DaHeader) -> Result<()> {
	let handle = db
		.cf_handle(BLOCK_HEADER_CF)
		.ok_or_else(|| eyre!("Failed to get cf handle"))?;

	db.put_cf(
		&handle,
		block_number.to_be_bytes(),
		serde_json::to_string(header)?.as_bytes(),
	)
	.wrap_err("Failed to write block header")
}

/// Checks if confidence factor for given block number is in database
pub fn is_confidence_in_db(db: Arc<DB>, block_number: u32) -> Result<bool> {
	let handle = db
		.cf_handle(CONFIDENCE_FACTOR_CF)
		.ok_or_else(|| eyre!("Failed to get cf handle"))?;

	db.get_pinned_cf(&handle, block_number.to_be_bytes())
		.wrap_err("Failed to get confidence")
		.map(|value| value.is_some())
}

pub trait DatabaseOld: Clone + Send {
	fn get_confidence(&self, block_number: u32) -> Result<Option<u32>>;
	fn get_header(&self, block_number: u32) -> Result<Option<DaHeader>>;
	fn get_data(&self, app_id: u32, block_number: u32) -> Result<Option<AppData>>;
}

#[derive(Clone)]
pub struct RocksDBOld(pub Arc<DB>);

impl DatabaseOld for RocksDBOld {
	fn get_confidence(&self, block_number: u32) -> Result<Option<u32>> {
		get_confidence_from_db(self.0.clone(), block_number)
	}

	fn get_header(&self, block_number: u32) -> Result<Option<DaHeader>> {
		get_block_header_from_db(self.0.clone(), block_number)
	}

	fn get_data(&self, app_id: u32, block_number: u32) -> Result<Option<AppData>> {
		get_decoded_data_from_db(self.0.clone(), app_id, block_number)
	}
}

/// Gets confidence factor from database for given block number
pub fn get_confidence_from_db(db: Arc<DB>, block_number: u32) -> Result<Option<u32>> {
	let cf_handle = db
		.cf_handle(crate::consts::CONFIDENCE_FACTOR_CF)
		.ok_or_else(|| eyre!("Couldn't get column handle from db"))?;

	db.get_cf(&cf_handle, block_number.to_be_bytes())
		.wrap_err("Couldn't get confidence in db")?
		.map(|data| {
			data.try_into()
				.map_err(|_| eyre!("Conversion failed"))
				.wrap_err("Unable to convert confidence (wrong number of bytes)")
				.map(u32::from_be_bytes)
		})
		.transpose()
}

/// Stores confidence factor into database under the given block number key
pub fn store_confidence_in_db(db: Arc<DB>, block_number: u32, count: u32) -> Result<()> {
	let handle = db
		.cf_handle(CONFIDENCE_FACTOR_CF)
		.ok_or_else(|| eyre!("Failed to get cf handle"))?;

	db.put_cf(&handle, block_number.to_be_bytes(), count.to_be_bytes())
		.wrap_err("Failed to write confidence")
}

pub fn get_finality_sync_checkpoint(db: Arc<DB>) -> Result<Option<FinalitySyncCheckpoint>> {
	let cf_handle = db
		.cf_handle(STATE_CF)
		.ok_or_else(|| eyre!("Couldn't get column handle from db"))?;

	let result = db
		.get_cf(&cf_handle, FINALITY_SYNC_CHECKPOINT_KEY.as_bytes())
		.wrap_err("Couldn't get finality sync checkpoint from db")?;

	result.map_or(Ok(None), |e| {
		FinalitySyncCheckpoint::decode(&mut &e[..])
			.wrap_err("Failed to decoded finality sync checkpoint")
			.map(Some)
	})
}

pub fn store_finality_sync_checkpoint(
	db: Arc<DB>,
	checkpoint: FinalitySyncCheckpoint,
) -> Result<()> {
	let cf_handle = db
		.cf_handle(STATE_CF)
		.ok_or_else(|| eyre!("Couldn't get column handle from db"))?;
	db.put_cf(
		&cf_handle,
		FINALITY_SYNC_CHECKPOINT_KEY.as_bytes(),
		checkpoint.encode().as_slice(),
	)
	.wrap_err("Failed to write finality sync checkpoint data")
}

pub trait Database: Send + Sync {
	fn put(&self, column_family: Option<&str>, key: &[u8], value: &[u8]) -> Result<()>;
	fn get(&self, column_family: Option<&str>, key: &[u8]) -> Result<Option<Vec<u8>>>;
	fn delete(&self, column_family: Option<&str>, key: &[u8]) -> Result<()>;
}

pub struct RocksDB {
	db: Arc<rocksdb::DB>,
}

impl RocksDB {
	pub fn open(path: &str) -> Result<RocksDB> {
		let cf_opts = vec![
			ColumnFamilyDescriptor::new(CONFIDENCE_FACTOR_CF, Options::default()),
			ColumnFamilyDescriptor::new(BLOCK_HEADER_CF, Options::default()),
			ColumnFamilyDescriptor::new(APP_DATA_CF, Options::default()),
			ColumnFamilyDescriptor::new(STATE_CF, Options::default()),
		];

		let mut db_opts = Options::default();
		db_opts.create_if_missing(true);
		db_opts.create_missing_column_families(true);

		let db = rocksdb::DB::open_cf_descriptors(&db_opts, path, cf_opts)?;
		Ok(RocksDB { db: Arc::new(db) })
	}
}

impl Database for RocksDB {
	fn put(&self, column_family: Option<&str>, key: &[u8], value: &[u8]) -> Result<()> {
		// if Column Family descriptor was provided, put the key in that partition
		let Some(cf) = column_family else {
			// else, just put it in the default partition
			return self
				.db
				.put(key, value)
				.wrap_err("Put operation failed on RocksDB");
		};
		let cf_handle = self
			.db
			.cf_handle(cf)
			.ok_or_else(|| eyre!("Couldn't get Column Family handle from RocksDB"))?;
		self.db
			.put_cf(&cf_handle, key, value)
			.wrap_err("Put operation with Column Family failed on RocksDB")
	}

	fn get(&self, column_family: Option<&str>, key: &[u8]) -> Result<Option<Vec<u8>>> {
		// if Column Family descriptor was provided, get the key from that partition
		let Some(cf) = column_family else {
			// else, just get it from the default partition
			return self.db.get(key).wrap_err("Get operation failed on RocksDB");
		};
		let cf_handle = self
			.db
			.cf_handle(cf)
			.ok_or_else(|| eyre!("Couldn't get Column Family handle from RocksDB"))?;
		self.db
			.get_cf(&cf_handle, key)
			.wrap_err("Get operation with Column Family failed on RocksDB")
	}

	fn delete(&self, column_family: Option<&str>, key: &[u8]) -> Result<()> {
		// if Column Family descriptor was provided, delete the key from that partition
		let Some(cf) = column_family else {
			// else, just delete it from the default partition
			return self
				.db
				.delete(key)
				.wrap_err("Delete operation failed on RocksDB");
		};
		let cf_handle = self
			.db
			.cf_handle(cf)
			.ok_or_else(|| eyre!("Couldn't get Column Family handle from RocksDB"))?;
		self.db
			.delete_cf(&cf_handle, key)
			.wrap_err("Delete operation with Column Family failed on RocksDB")
	}
}

// Generic struct that uses any type implementing the Database trait
#[derive(Clone)]
pub struct DataManager<D: Database + Clone> {
	db: D,
}

impl<D: Database + Clone> DataManager<D> {
	pub fn new(db: D) -> DataManager<D> {
		DataManager { db }
	}

	pub fn store_app_data(&self, app_id: AppId, block_number: u32, data: &AppData) -> Result<()> {
		let key = format!("{}:{block_number}", app_id.0);
		self.db
			.put(Some(APP_DATA_CF), key.as_bytes(), &data.encode())
			.wrap_err("Failed to store App Data in DB")
	}

	/// Gets and decodes app data from database for the `app_id:block_number` key
	pub fn get_app_data(&self, app_id: u32, block_number: u32) -> Result<Option<AppData>> {
		let key = format!("{app_id}:{block_number}");
		let result = self
			.db
			.get(Some(APP_DATA_CF), key.as_bytes())?
			.map(|v| AppData::decode(&mut &v[..]));

		match result {
			Some(Err(e)) => Err(eyre!("Failed to decode Extrinsics Data: {e}")),
			Some(Ok(data)) => Ok(Some(data)),
			None => Ok(None),
		}
	}
}
