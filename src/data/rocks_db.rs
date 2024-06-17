use super::{keys::*, *};
use crate::{
	data::{self, APP_STATE_CF, KADEMLIA_STORE_CF},
	network::p2p::ExpirationCompactionFilterFactory,
};
use codec::{Decode, Encode};
use color_eyre::eyre::{eyre, Context, Result};
use rocksdb::{ColumnFamilyDescriptor, Options};
use std::sync::Arc;

#[derive(Clone)]
pub struct RocksDB {
	db: Arc<rocksdb::DB>,
}

#[derive(Eq, Hash, PartialEq)]
pub struct RocksDBKey(Option<&'static str>, Vec<u8>);

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
		let RocksDBKey(column_family, key) = key.into();
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
		let RocksDBKey(column_family, key) = key.into();
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
		let RocksDBKey(column_family, key) = key.into();
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

impl From<AppDataKey> for RocksDBKey {
	fn from(value: AppDataKey) -> Self {
		let AppDataKey(app_id, block_num) = value;
		RocksDBKey(
			Some(APP_STATE_CF),
			format!("{APP_ID_PREFIX}:{app_id}:{block_num}").into_bytes(),
		)
	}
}

impl From<BlockHeaderKey> for RocksDBKey {
	fn from(value: BlockHeaderKey) -> Self {
		let BlockHeaderKey(block_num) = value;
		RocksDBKey(
			Some(APP_STATE_CF),
			format!("{BLOCK_HEADER_KEY_PREFIX}:{block_num}").into_bytes(),
		)
	}
}

impl From<VerifiedCellCountKey> for RocksDBKey {
	fn from(value: VerifiedCellCountKey) -> Self {
		let VerifiedCellCountKey(count) = value;
		RocksDBKey(
			Some(APP_STATE_CF),
			format!("{VERIFIED_CELL_COUNT_PREFIX}:{count}").into_bytes(),
		)
	}
}

impl From<FinalitySyncCheckpointKey> for RocksDBKey {
	fn from(_: FinalitySyncCheckpointKey) -> Self {
		RocksDBKey(
			Some(APP_STATE_CF),
			FINALITY_SYNC_CHECKPOINT_KEY.as_bytes().to_vec(),
		)
	}
}

impl From<RpcNodeKey> for RocksDBKey {
	fn from(_: RpcNodeKey) -> Self {
		RocksDBKey(
			Some(APP_STATE_CF),
			CONNECTED_RPC_NODE_KEY.as_bytes().to_vec(),
		)
	}
}

impl From<IsFinalitySyncedKey> for RocksDBKey {
	fn from(_: IsFinalitySyncedKey) -> Self {
		RocksDBKey(
			Some(APP_STATE_CF),
			IS_FINALITY_SYNCED_KEY.as_bytes().to_vec(),
		)
	}
}

impl From<VerifiedSyncDataKey> for RocksDBKey {
	fn from(_: VerifiedSyncDataKey) -> Self {
		RocksDBKey(Some(APP_STATE_CF), VERIFIED_SYNC_DATA.as_bytes().to_vec())
	}
}

impl From<AchievedSyncConfidenceKey> for RocksDBKey {
	fn from(_: AchievedSyncConfidenceKey) -> Self {
		RocksDBKey(
			Some(APP_STATE_CF),
			ACHIEVED_SYNC_CONFIDENCE_KEY.as_bytes().to_vec(),
		)
	}
}

impl From<VerifiedSyncHeaderKey> for RocksDBKey {
	fn from(_: VerifiedSyncHeaderKey) -> Self {
		RocksDBKey(
			Some(APP_STATE_CF),
			VERIFIED_SYNC_HEADER_KEY.as_bytes().to_vec(),
		)
	}
}

impl From<LatestSyncKey> for RocksDBKey {
	fn from(_: LatestSyncKey) -> Self {
		RocksDBKey(Some(APP_STATE_CF), LATEST_SYNC_KEY.as_bytes().to_vec())
	}
}

impl From<VerifiedDataKey> for RocksDBKey {
	fn from(_: VerifiedDataKey) -> Self {
		RocksDBKey(Some(APP_STATE_CF), VERIFIED_DATA_KEY.as_bytes().to_vec())
	}
}
impl From<AchievedConfidenceKey> for RocksDBKey {
	fn from(_: AchievedConfidenceKey) -> Self {
		RocksDBKey(
			Some(APP_STATE_CF),
			ACHIEVED_CONFIDENCE_KEY.as_bytes().to_vec(),
		)
	}
}

impl From<VerifiedHeaderKey> for RocksDBKey {
	fn from(_: VerifiedHeaderKey) -> Self {
		RocksDBKey(Some(APP_STATE_CF), VERIFIED_HEADER_KEY.as_bytes().to_vec())
	}
}

impl From<LatestHeaderKey> for RocksDBKey {
	fn from(_: LatestHeaderKey) -> Self {
		RocksDBKey(Some(APP_STATE_CF), LATEST_HEADER_KEY.as_bytes().to_vec())
	}
}

impl From<IsSyncedKey> for RocksDBKey {
	fn from(_: IsSyncedKey) -> Self {
		RocksDBKey(Some(APP_STATE_CF), IS_SYNCED_KEY.as_bytes().to_vec())
	}
}
