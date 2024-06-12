use crate::data::Database;
use color_eyre::eyre::{eyre, Result};
use std::{
	collections::HashMap,
	sync::{Arc, RwLock},
};

use super::{HashMapKey, RecordKey};

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
