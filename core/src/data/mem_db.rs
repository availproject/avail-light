use crate::data::Database;
use std::{
	collections::HashMap,
	sync::{Arc, RwLock},
};

use super::RecordKey;

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

impl<T: RecordKey> From<T> for HashMapKey {
	fn from(value: T) -> Self {
		let key = value.key();
		HashMapKey(match value.space() {
			Some(space) => format!("{space}::{key}"),
			None => key,
		})
	}
}

impl Database for MemoryDB {
	fn put<T: RecordKey>(&self, key: T, value: T::Type) {
		let mut map = self.map.write().expect("Lock acquired");

		map.insert(
			key.into(),
			serde_json::to_string(&value).expect("Encoding data for MemoryDB failed"),
		);
	}

	fn get<T: RecordKey>(&self, key: T) -> Option<T::Type> {
		let map = self.map.read().expect("Lock acquired");
		map.get(&key.into())
			.map(|value| serde_json::from_str(value).expect("Decoding data from MemoryDB failed"))
	}

	fn delete<T: RecordKey>(&self, key: T) {
		let mut map = self.map.write().expect("Lock acquired");
		map.remove(&key.into());
	}
}
