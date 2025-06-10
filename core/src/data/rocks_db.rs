use crate::{
	data::{self, APP_STATE_CF, KADEMLIA_STORE_CF},
	network::p2p::ExpirationCompactionFilterFactory,
};
use codec::{Decode, Encode};
use color_eyre::eyre::Result;
use rocksdb::{ColumnFamilyDescriptor, DBCompressionType, Options};
use std::sync::Arc;

use super::RecordKey;

#[derive(Clone)]
pub struct RocksDB {
	db: Arc<rocksdb::DB>,
}

#[derive(Eq, Hash, PartialEq)]
pub struct RocksDBKey(Option<&'static str>, Vec<u8>);

impl<T: RecordKey> From<T> for RocksDBKey {
	fn from(value: T) -> Self {
		RocksDBKey(value.space(), value.key().into_bytes())
	}
}

impl RocksDB {
	pub fn open(path: &str) -> Result<RocksDB> {
		let mut kademlia_store_cf_opts = Options::default();
		kademlia_store_cf_opts
			.set_compaction_filter_factory(ExpirationCompactionFilterFactory::default());
		kademlia_store_cf_opts.set_compression_type(DBCompressionType::Lz4);
		kademlia_store_cf_opts.set_optimize_filters_for_hits(true);

		let cf_opts = vec![
			ColumnFamilyDescriptor::new(APP_STATE_CF, Options::default()),
			ColumnFamilyDescriptor::new(KADEMLIA_STORE_CF, kademlia_store_cf_opts),
		];

		let mut db_opts = Options::default();
		db_opts.create_if_missing(true);
		db_opts.create_missing_column_families(true);

		let db = Arc::new(rocksdb::DB::open_cf_descriptors(&db_opts, path, cf_opts)?);
		Ok(RocksDB { db })
	}

	pub fn inner(&self) -> Arc<rocksdb::DB> {
		self.db.clone()
	}
}

impl data::Database for RocksDB {
	fn put<T: RecordKey>(&self, key: T, value: T::Type) {
		let RocksDBKey(column_family, key) = key.into();
		// if Column Family descriptor was provided, put the key in that partition
		let Some(cf) = column_family else {
			// else, just put it in the default partition
			return self
				.db
				.put(key, <T::Type>::encode(&value))
				.expect("Put operation has failed on RocksDB");
		};

		let cf_handle = self
			.db
			.cf_handle(cf)
			.expect("Couldn't get Column Family handle from RocksDB");
		self.db
			.put_cf(&cf_handle, key, <T::Type>::encode(&value))
			.expect("Put operation with Column Family has failed on RocksDB")
	}

	fn get<T: RecordKey>(&self, key: T) -> Option<T::Type> {
		let RocksDBKey(column_family, key) = key.into();
		// if Column Family descriptor was provided, get the key from that partition
		let Some(cf) = column_family else {
			// else, just get it from the default partition
			return self
				.db
				.get(key)
				.expect("Get operation has failed on RocksDB")
				.map(|value| {
					<T::Type>::decode(&mut &value[..]).expect("Failed to decode the RocksDB data.")
				});
		};

		let cf_handle = self
			.db
			.cf_handle(cf)
			.expect("Couldn't get Column Family handle from RocksDB");

		self.db
			.get_cf(&cf_handle, key)
			.expect("Couldn't get Column Family handle from RocksDB")
			.map(|value| {
				<T::Type>::decode(&mut &value[..]).expect("Failed to decode the RocksDB data.")
			})
	}

	fn delete<T: RecordKey>(&self, key: T) {
		let RocksDBKey(column_family, key) = key.into();
		// if Column Family descriptor was provided, delete the key from that partition
		let Some(cf) = column_family else {
			// else, just delete it from the default partition
			return self
				.db
				.delete(key)
				.expect("Delete operation has failed on RocksDB");
		};
		let cf_handle = self
			.db
			.cf_handle(cf)
			.expect("Couldn't get Column Family handle from RocksDB");
		self.db
			.delete_cf(&cf_handle, key)
			.expect("Delete operation with Column Family has failed on RocksDB")
	}
}
