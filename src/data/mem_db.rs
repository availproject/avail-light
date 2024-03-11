use crate::data::{
	Database, Key, APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF, FINALITY_SYNC_CHECKPOINT_KEY,
};
use color_eyre::eyre::{eyre, Result};
use serde::{Deserialize, Serialize};
use std::{
	collections::HashMap,
	sync::{Arc, RwLock},
};

#[derive(Eq, Hash, PartialEq)]
pub struct HashMapKey(pub String);

#[derive(Clone)]
pub struct MemoryDB {
	map: Arc<RwLock<HashMap<HashMapKey, String>>>,
}

impl Default for MemoryDB {
	fn default() -> Self {
		MemoryDB {
			map: Arc::new(RwLock::new(HashMap::new())),
		}
	}
}

impl Database for MemoryDB {
	type Key = HashMapKey;
	fn put<T>(&self, key: Key, value: T) -> Result<()>
	where
		T: Serialize,
	{
		let mut map = self.map.write().expect("Lock acquired");

		map.insert(key.into(), serde_json::to_string(&value)?);
		Ok(())
	}

	fn get<T>(&self, key: Key) -> Result<Option<T>>
	where
		T: for<'a> Deserialize<'a>,
	{
		let map = self.map.read().expect("Lock acquired");
		map.get(&key.into())
			.map(|value| serde_json::from_str(value).map_err(|error| eyre!("{error}")))
			.transpose()
	}

	fn delete(&self, key: Key) -> Result<()> {
		let mut map = self.map.write().expect("Lock acquired");
		map.remove(&key.into());
		Ok(())
	}
}

impl From<Key> for HashMapKey {
	fn from(key: Key) -> Self {
		match key {
			Key::AppData(app_id, block_number) => {
				HashMapKey(format!("{APP_DATA_CF}:{app_id}:{block_number}"))
			},
			Key::BlockHeader(block_number) => {
				HashMapKey(format!("{BLOCK_HEADER_CF}:{block_number}"))
			},
			Key::VerifiedCellCount(block_number) => {
				HashMapKey(format!("{CONFIDENCE_FACTOR_CF}:{block_number}"))
			},
			Key::FinalitySyncCheckpoint => HashMapKey(FINALITY_SYNC_CHECKPOINT_KEY.to_string()),
		}
	}
}
