pub mod data {
	use color_eyre::eyre::Result;

	/// Types that can be decoded from T
	pub trait Decode<T>: Sized {
		fn decode(source: T) -> Result<Self>;
	}

	/// Types that can be encoded into T
	pub trait Encode<T> {
		fn encode(&self) -> Result<T>;
	}

	pub trait DB<K>
	where
		K: Into<Self::Key>,
	{
		/// Type of the database key which we can get from the custom key.
		type Key;

		/// Type of the result of the database query.
		type Result;

		/// Puts value for given key into database.
		/// Key is encoded into database key, value is encoded into type supported by database.
		fn put<T>(&self, key: K, value: T) -> Result<()>
		where
			T: Encode<Self::Result>;

		/// Gets value for given key.
		/// Key is encoded into database key, value is decoded into the given type.
		fn get<T>(&self, key: K) -> Result<Option<T>>
		where
			T: Decode<Self::Result>;

		/// Deletes value from the database for the given key.
		fn delete(&self, key: K) -> Result<()>;
	}

	pub mod rocks {
		use crate::{
			data::{APP_DATA_CF, BLOCK_HEADER_CF, CONFIDENCE_FACTOR_CF, STATE_CF},
			db::data,
		};
		use color_eyre::eyre::{eyre, Context, Result};
		use rocksdb::{ColumnFamilyDescriptor, Options};
		use std::sync::Arc;

		#[derive(Clone)]
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

		type RocksKey = (Option<&'static str>, Vec<u8>);

		impl<K: Into<RocksKey>> data::DB<K> for RocksDB {
			type Key = RocksKey;
			type Result = Vec<u8>;

			fn put<T>(&self, key: K, value: T) -> Result<()>
			where
				T: data::Encode<Self::Result>,
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
					.ok_or_else(|| eyre!("Couldn't get Column Family handle from RocksDB"))?;
				self.db
					.put_cf(&cf_handle, key, value.encode()?)
					.wrap_err("Put operation with Column Family failed on RocksDB")
			}

			fn get<T>(&self, key: K) -> Result<Option<T>>
			where
				T: data::Decode<Self::Result>,
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
					.ok_or_else(|| eyre!("Couldn't get Column Family handle from RocksDB"))?;

				self.db
					.get_cf(&cf_handle, key)?
					.map(T::decode)
					.transpose()
					.wrap_err("Get operation with Column Family failed on RocksDB")
			}

			fn delete(&self, key: K) -> Result<()> {
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
	}

	// pub mod mem {
	// 	use crate::db::data::{Decode, Encode, DB};
	// 	use color_eyre::eyre::Result;
	// 	use std::{collections::HashMap, sync::RwLock};

	// 	#[derive(Eq, Hash, PartialEq)]
	// 	pub struct HashMapKey(pub String);

	// 	pub struct MemoryDB {
	// 		map: RwLock<HashMap<HashMapKey, String>>,
	// 	}

	// 	impl Default for MemoryDB {
	// 		fn default() -> Self {
	// 			MemoryDB {
	// 				map: RwLock::new(HashMap::new()),
	// 			}
	// 		}
	// 	}

	// 	impl<K: Into<HashMapKey>> DB<K> for MemoryDB {
	// 		type Key = HashMapKey;
	// 		type Result = String;

	// 		fn put<T>(&self, key: K, value: T) -> Result<()>
	// 		where
	// 			T: Encode<Self::Result>,
	// 		{
	// 			let mut map = self.map.write().expect("Lock acquired");
	// 			map.insert(key.into(), value.encode()?);
	// 			Ok(())
	// 		}

	// 		fn get<T>(&self, key: K) -> Result<Option<T>>
	// 		where
	// 			T: Decode<Self::Result>,
	// 		{
	// 			let map = self.map.read().expect("Lock acquired");
	// 			map.get(&key.into()).cloned().map(T::decode).transpose()
	// 		}

	// 		fn delete(&self, key: K) -> Result<()> {
	// 			let mut map = self.map.write().expect("Lock acquired");
	// 			map.remove(&key.into());
	// 			Ok(())
	// 		}
	// 	}
	// }

	// pub mod domain {
	// 	use super::{mem::HashMapKey, Decode, Encode};
	// 	use crate::db::data::DB;
	// 	use color_eyre::eyre::Result;
	// 	use serde::{Deserialize, Serialize};

	// 	#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
	// 	pub struct One {
	// 		pub one: String,
	// 	}

	// 	impl Encode<String> for One {
	// 		fn encode(&self) -> Result<String> {
	// 			Ok(serde_json::to_string(self).unwrap())
	// 		}
	// 	}

	// 	impl Decode<String> for One {
	// 		fn decode(source: String) -> Result<Self> {
	// 			Ok(serde_json::from_str(&source).unwrap())
	// 		}
	// 	}

	// 	impl Encode<Vec<u8>> for One {
	// 		fn encode(&self) -> Result<Vec<u8>> {
	// 			Ok(serde_json::to_vec(self).unwrap())
	// 		}
	// 	}

	// 	impl Decode<Vec<u8>> for One {
	// 		fn decode(source: Vec<u8>) -> Result<Self> {
	// 			Ok(serde_json::from_slice(&source).unwrap())
	// 		}
	// 	}

	// 	#[derive(Serialize, Deserialize)]
	// 	pub struct Two {
	// 		pub one: String,
	// 		pub two: String,
	// 	}

	// 	impl Encode<String> for Two {
	// 		fn encode(&self) -> Result<String> {
	// 			Ok(serde_json::to_string(self).unwrap())
	// 		}
	// 	}

	// 	impl Decode<String> for Two {
	// 		fn decode(source: String) -> Result<Self> {
	// 			Ok(serde_json::from_str(&source).unwrap())
	// 		}
	// 	}

	// 	#[derive(Clone)]
	// 	pub enum Key {
	// 		One(String),
	// 		Two((String, String)),
	// 	}

	// 	impl From<Key> for HashMapKey {
	// 		fn from(key: Key) -> Self {
	// 			match key {
	// 				Key::One(key) => HashMapKey(key),
	// 				Key::Two((key1, key2)) => HashMapKey(format!("{key1}:{key2}")),
	// 			}
	// 		}
	// 	}

	// 	impl From<Key> for (Option<&'static str>, Vec<u8>) {
	// 		fn from(key: Key) -> Self {
	// 			match key {
	// 				Key::One(key) => (None, key.into_bytes()),
	// 				Key::Two((key1, key2)) => (None, format!("{key1}:{key2}").into_bytes()),
	// 			}
	// 		}
	// 	}

	// 	pub struct Server<T> {
	// 		pub db: T,
	// 	}

	// 	impl<T> Server<T>
	// 	where
	// 		T: DB<Key>,
	// 		Key: Into<T::Key>,
	// 	{
	// 		pub fn get_one(&self, key: Key) -> Result<Option<One>>
	// 		where
	// 			One: Decode<T::Result>,
	// 		{
	// 			self.db.get(key)
	// 		}

	// 		pub fn put_one(&self, key: Key, one: One) -> Result<()>
	// 		where
	// 			One: Encode<T::Result>,
	// 		{
	// 			self.db.put(key, one)
	// 		}

	// 		pub fn get_two(&self, key: Key) -> Result<Option<Two>>
	// 		where
	// 			Two: Decode<T::Result>,
	// 		{
	// 			self.db.get(key)
	// 		}

	// 		pub fn put_two(&self, key: Key, two: Two) -> Result<()>
	// 		where
	// 			Two: Encode<T::Result>,
	// 		{
	// 			self.db.put(key, two)
	// 		}

	// 		pub fn delete(&self, key: Key) -> Result<()> {
	// 			self.db.delete(key)
	// 		}
	// 	}
	// }
}

// #[cfg(test)]
// mod tests {
// 	use super::data::{
// 		domain::{Key, One, Server},
// 		mem::MemoryDB,
// 	};
// 	use crate::db::data::{domain::Two, rocks::RocksDB};

// 	#[test]
// 	fn main_test() {
// 		let key_one_first = Key::One("one_first".to_string());
// 		let key_one_second = Key::One("one_second".to_string());
// 		let key_two = Key::Two(("one".to_string(), "two".to_string()));
// 		let one_first = One {
// 			one: "one_first".to_string(),
// 		};
// 		let two = Two {
// 			one: "one".to_string(),
// 			two: "two".to_string(),
// 		};
// 		let mem_db = MemoryDB::default();
// 		let server_1 = Server { db: mem_db };
// 		server_1
// 			.put_one(key_one_first.clone(), one_first.clone())
// 			.unwrap();
// 		_ = server_1.put_two(key_two.clone(), two);

// 		let result = server_1.get_one(key_one_first.clone()).unwrap().unwrap();
// 		assert_eq!(result, one_first);
// 		let result_2 = server_1.get_one(key_one_second.clone()).unwrap();
// 		assert_eq!(result_2, None);
// 		_ = server_1.delete(key_one_first.clone());
// 		let result_3 = server_1.get_two(key_two).unwrap().unwrap();
// 		assert_eq!(result_3.one, "one");
// 		assert_eq!(result_3.two, "two");

// 		let rocks_db = RocksDB::open("temporary").unwrap();
// 		let server_2 = Server { db: rocks_db };
// 		server_2
// 			.put_one(key_one_first.clone(), one_first.clone())
// 			.unwrap();

// 		let result = server_2.get_one(key_one_first.clone()).unwrap().unwrap();
// 		assert_eq!(result, one_first);
// 		let result_2 = server_2.get_one(key_one_second).unwrap();
// 		assert_eq!(result_2, None);
// 		_ = server_2.delete(key_one_first);
// 	}
// }
