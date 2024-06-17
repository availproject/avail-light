use super::{keys::*, *};
use crate::data::Database;
use color_eyre::eyre::{eyre, Result};
use std::{
	collections::HashMap,
	sync::{Arc, RwLock},
};

#[derive(Clone)]
pub struct MemoryDB {
	map: Arc<RwLock<HashMap<HashMapKey, String>>>,
}

#[derive(Eq, Hash, PartialEq)]
pub struct HashMapKey(pub String);

impl Default for MemoryDB {
	fn default() -> Self {
		MemoryDB {
			map: Arc::new(RwLock::new(HashMap::new())),
		}
	}
}

impl Database for MemoryDB {
	fn put<T: RecordKey>(&self, key: T, value: T::Type) -> Result<()> {
		let mut map = self.map.write().expect("Lock acquired");

		map.insert(key.into(), serde_json::to_string(&value)?);
		Ok(())
	}

	fn get<T: RecordKey>(&self, key: T) -> Result<Option<T::Type>> {
		let map = self.map.read().expect("Lock acquired");
		map.get(&key.into())
			.map(|value| serde_json::from_str(value).map_err(|error| eyre!("{error}")))
			.transpose()
	}

	fn delete<T: RecordKey>(&self, key: T) -> Result<()> {
		let mut map = self.map.write().expect("Lock acquired");
		map.remove(&key.into());
		Ok(())
	}
}

impl From<AppDataKey> for HashMapKey {
	fn from(value: AppDataKey) -> Self {
		let AppDataKey(app_id, block_num) = value;
		HashMapKey(format!(
			"{APP_STATE_CF}:{APP_ID_PREFIX}:{app_id}:{block_num}"
		))
	}
}

impl From<BlockHeaderKey> for HashMapKey {
	fn from(value: BlockHeaderKey) -> Self {
		let BlockHeaderKey(block_num) = value;
		HashMapKey(format!(
			"{APP_STATE_CF}:{BLOCK_HEADER_KEY_PREFIX}:{block_num}"
		))
	}
}

impl From<VerifiedCellCountKey> for HashMapKey {
	fn from(value: VerifiedCellCountKey) -> Self {
		let VerifiedCellCountKey(block_num) = value;
		HashMapKey(format!(
			"{APP_STATE_CF}:{VERIFIED_CELL_COUNT_PREFIX}:{block_num}"
		))
	}
}

impl From<FinalitySyncCheckpointKey> for HashMapKey {
	fn from(_: FinalitySyncCheckpointKey) -> Self {
		HashMapKey(FINALITY_SYNC_CHECKPOINT_KEY.to_string())
	}
}

impl From<RpcNodeKey> for HashMapKey {
	fn from(_: RpcNodeKey) -> Self {
		HashMapKey(CONNECTED_RPC_NODE_KEY.to_string())
	}
}

impl From<IsFinalitySyncedKey> for HashMapKey {
	fn from(_: IsFinalitySyncedKey) -> Self {
		HashMapKey(IS_FINALITY_SYNCED_KEY.to_string())
	}
}

impl From<VerifiedSyncDataKey> for HashMapKey {
	fn from(_: VerifiedSyncDataKey) -> Self {
		HashMapKey(VERIFIED_SYNC_DATA.to_string())
	}
}

impl From<AchievedSyncConfidenceKey> for HashMapKey {
	fn from(_: AchievedSyncConfidenceKey) -> Self {
		HashMapKey(ACHIEVED_SYNC_CONFIDENCE_KEY.to_string())
	}
}

impl From<VerifiedSyncHeaderKey> for HashMapKey {
	fn from(_: VerifiedSyncHeaderKey) -> Self {
		HashMapKey(VERIFIED_SYNC_HEADER_KEY.to_string())
	}
}

impl From<LatestSyncKey> for HashMapKey {
	fn from(_: LatestSyncKey) -> Self {
		HashMapKey(LATEST_SYNC_KEY.to_string())
	}
}

impl From<VerifiedDataKey> for HashMapKey {
	fn from(_: VerifiedDataKey) -> Self {
		HashMapKey(VERIFIED_DATA_KEY.to_string())
	}
}

impl From<AchievedConfidenceKey> for HashMapKey {
	fn from(_: AchievedConfidenceKey) -> Self {
		HashMapKey(ACHIEVED_CONFIDENCE_KEY.to_string())
	}
}

impl From<VerifiedHeaderKey> for HashMapKey {
	fn from(_: VerifiedHeaderKey) -> Self {
		HashMapKey(VERIFIED_HEADER_KEY.to_string())
	}
}

impl From<LatestHeaderKey> for HashMapKey {
	fn from(_: LatestHeaderKey) -> Self {
		HashMapKey(LATEST_HEADER_KEY.to_string())
	}
}

impl From<IsSyncedKey> for HashMapKey {
	fn from(_: IsSyncedKey) -> Self {
		HashMapKey(IS_SYNCED_KEY.to_string())
	}
}
