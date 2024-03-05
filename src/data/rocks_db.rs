use color_eyre::eyre::{Context, ContextCompat, Result};
use rocksdb::{ColumnFamilyDescriptor, Options};
use std::sync::Arc;

use super::{
	database::{Database, Decode, Encode},
	APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF, STATE_CF,
};

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
	type Key = (Option<&'static str>, Vec<u8>);
	type Result = Vec<u8>;

	fn put<K, T>(&self, key: K, value: T) -> Result<()>
	where
		K: Into<Self::Key>,
		T: Encode<Self::Result>,
	{
		let (column_family, key) = key.into();
		// if Column Family descriptor was provided, put the key in that partition
		let Some(cf) = column_family else {
			// else, just put it in the default partition
			return self
				.db
				.put(key, value.encode()?)
				.wrap_err("Put operation failed on RocksDB");
		};

		let cf_handle = self
			.db
			.cf_handle(cf)
			.wrap_err("Couldn't get Column Family handle from RocksDB")?;

		self.db
			.put_cf(&cf_handle, key, value.encode()?)
			.wrap_err("Put operation with Column Family failed on RocksDB")
	}

	fn get<K, T>(&self, key: K) -> Result<Option<T>>
	where
		K: Into<Self::Key>,
		T: Decode<Self::Result>,
	{
		let (column_family, key) = key.into();
		// if Column Family descriptor was provided, get the key from that partition
		let Some(cf) = column_family else {
			// else, just get it from the default partition
			return self
				.db
				.get(key)?
				.map(T::decode)
				.transpose()
				.wrap_err("Get operation failed on RocksDB");
		};

		let cf_handle = self
			.db
			.cf_handle(cf)
			.wrap_err("Couldn't get Column Family handle from RocksDB")?;

		self.db
			.get_cf(&cf_handle, key)?
			.map(T::decode)
			.transpose()
			.wrap_err("Get operation with Column Family failed on RocksDB")
	}

	fn delete<K>(&self, key: K) -> Result<()>
	where
		K: Into<Self::Key>,
	{
		let (column_family, key) = key.into();
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
			.wrap_err("Couldn't get Column Family handle from RocksDB")?;

		self.db
			.delete_cf(&cf_handle, key)
			.wrap_err("Delete operation with Column Family failed on RocksDB")
	}
}
