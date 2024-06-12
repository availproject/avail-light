use crate::{
	data::{self, APP_STATE_CF, KADEMLIA_STORE_CF},
	network::p2p::ExpirationCompactionFilterFactory,
};
use codec::{Decode, Encode};
use color_eyre::eyre::{eyre, Context, Result};
use rocksdb::{ColumnFamilyDescriptor, Options};
use std::sync::Arc;

use super::RecordKey;

#[derive(Clone)]
pub struct RocksDB {
	db: Arc<rocksdb::DB>,
}

impl RocksDB {
	pub fn open(path: &str) -> Result<(RocksDB, Arc<rocksdb::DB>)> {
		let mut kademlia_store_cf_opts = Options::default();
		kademlia_store_cf_opts
			.set_compaction_filter_factory(ExpirationCompactionFilterFactory::default());
		let cf_opts = vec![
			ColumnFamilyDescriptor::new(APP_STATE_CF, Options::default()),
			ColumnFamilyDescriptor::new(KADEMLIA_STORE_CF, kademlia_store_cf_opts),
		];

		let mut db_opts = Options::default();
		db_opts.create_if_missing(true);
		db_opts.create_missing_column_families(true);

		let db = Arc::new(rocksdb::DB::open_cf_descriptors(&db_opts, path, cf_opts)?);
		Ok((RocksDB { db: db.clone() }, db))
	}
}

impl data::Database for RocksDB {
	fn put<T: RecordKey>(&self, key: T, value: T::Type) -> Result<()> {
		let (column_family, key) = key.into();
		// if Column Family descriptor was provided, put the key in that partition
		let Some(cf) = column_family else {
			// else, just put it in the default partition
			return self
				.db
				.put(key, <T::Type>::encode(&value))
				.wrap_err("Put operation failed on RocksDB");
		};

		let cf_handle = self
			.db
			.cf_handle(cf)
			.ok_or_else(|| eyre!("Couldn't get Column Family handle from RocksDB"))?;
		self.db
			.put_cf(&cf_handle, key, <T::Type>::encode(&value))
			.wrap_err("Put operation with Column Family failed on RocksDB")
	}

	fn get<T: RecordKey>(&self, key: T) -> Result<Option<T::Type>> {
		let (column_family, key) = key.into();
		// if Column Family descriptor was provided, get the key from that partition
		let Some(cf) = column_family else {
			// else, just get it from the default partition
			return self
				.db
				.get(key)?
				.map(|value| {
					<T::Type>::decode(&mut &value[..]).wrap_err("Failed decoding the app data.")
				})
				.transpose()
				.wrap_err("Get operation failed on RocksDB");
		};

		let cf_handle = self
			.db
			.cf_handle(cf)
			.ok_or_else(|| eyre!("Couldn't get Column Family handle from RocksDB"))?;

		self.db
			.get_cf(&cf_handle, key)?
			.map(|value| {
				<T::Type>::decode(&mut &value[..]).wrap_err("Failed decoding the app data.")
			})
			.transpose()
			.wrap_err("Get operation with Column Family failed on RocksDB")
	}

	fn delete<T: RecordKey>(&self, key: T) -> Result<()> {
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
			.ok_or_else(|| eyre!("Couldn't get Column Family handle from RocksDB"))?;
		self.db
			.delete_cf(&cf_handle, key)
			.wrap_err("Delete operation with Column Family failed on RocksDB")
	}
}
