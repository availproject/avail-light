use super::{keys::*, *};
use crate::{
	data::{self, APP_STATE_CF, KADEMLIA_STORE_CF},
	network::p2p::ExpirationCompactionFilterFactory,
};
use codec::{Decode, Encode};
use color_eyre::eyre::Result;
use rocksdb::{ColumnFamilyDescriptor, Options};
use std::sync::Arc;

#[derive(Clone)]
pub struct RocksDB {
	db: Arc<rocksdb::DB>,
}

#[derive(Eq, Hash, PartialEq)]
pub struct RocksDBKey(Option<&'static str>, Vec<u8>);

impl RocksDBKey {
	fn app_state(key: &str) -> Self {
		Self(Some(APP_STATE_CF), key.as_bytes().to_vec())
	}
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

impl From<AppDataKey> for RocksDBKey {
	fn from(value: AppDataKey) -> Self {
		let AppDataKey(app_id, block_num) = value;
		RocksDBKey::app_state(&format!("{APP_ID_PREFIX}:{app_id}:{block_num}"))
	}
}

impl From<BlockHeaderKey> for RocksDBKey {
	fn from(value: BlockHeaderKey) -> Self {
		let BlockHeaderKey(block_num) = value;
		RocksDBKey::app_state(&format!("{BLOCK_HEADER_KEY_PREFIX}:{block_num}"))
	}
}

impl From<VerifiedCellCountKey> for RocksDBKey {
	fn from(value: VerifiedCellCountKey) -> Self {
		let VerifiedCellCountKey(count) = value;
		RocksDBKey::app_state(&format!("{VERIFIED_CELL_COUNT_PREFIX}:{count}"))
	}
}

impl From<FinalitySyncCheckpointKey> for RocksDBKey {
	fn from(_: FinalitySyncCheckpointKey) -> Self {
		RocksDBKey::app_state(FINALITY_SYNC_CHECKPOINT_KEY)
	}
}

impl From<RpcNodeKey> for RocksDBKey {
	fn from(_: RpcNodeKey) -> Self {
		RocksDBKey::app_state(CONNECTED_RPC_NODE_KEY)
	}
}

impl From<IsFinalitySyncedKey> for RocksDBKey {
	fn from(_: IsFinalitySyncedKey) -> Self {
		RocksDBKey::app_state(IS_FINALITY_SYNCED_KEY)
	}
}

impl From<VerifiedSyncDataKey> for RocksDBKey {
	fn from(_: VerifiedSyncDataKey) -> Self {
		RocksDBKey::app_state(VERIFIED_SYNC_DATA)
	}
}

impl From<AchievedSyncConfidenceKey> for RocksDBKey {
	fn from(_: AchievedSyncConfidenceKey) -> Self {
		RocksDBKey::app_state(ACHIEVED_SYNC_CONFIDENCE_KEY)
	}
}

impl From<VerifiedSyncHeaderKey> for RocksDBKey {
	fn from(_: VerifiedSyncHeaderKey) -> Self {
		RocksDBKey::app_state(VERIFIED_SYNC_HEADER_KEY)
	}
}

impl From<LatestSyncKey> for RocksDBKey {
	fn from(_: LatestSyncKey) -> Self {
		RocksDBKey::app_state(LATEST_SYNC_KEY)
	}
}

impl From<VerifiedDataKey> for RocksDBKey {
	fn from(_: VerifiedDataKey) -> Self {
		RocksDBKey::app_state(VERIFIED_DATA_KEY)
	}
}
impl From<AchievedConfidenceKey> for RocksDBKey {
	fn from(_: AchievedConfidenceKey) -> Self {
		RocksDBKey::app_state(ACHIEVED_CONFIDENCE_KEY)
	}
}

impl From<VerifiedHeaderKey> for RocksDBKey {
	fn from(_: VerifiedHeaderKey) -> Self {
		RocksDBKey::app_state(VERIFIED_HEADER_KEY)
	}
}

impl From<LatestHeaderKey> for RocksDBKey {
	fn from(_: LatestHeaderKey) -> Self {
		RocksDBKey::app_state(LATEST_HEADER_KEY)
	}
}

impl From<IsSyncedKey> for RocksDBKey {
	fn from(_: IsSyncedKey) -> Self {
		RocksDBKey::app_state(IS_SYNCED_KEY)
	}
}
