use codec::{Decode, Encode};
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};
use sp_core::ed25519;

pub mod keys;
pub mod rocks_db;

#[cfg(test)]
pub mod mem_db;

/// Column family for application state
pub const APP_STATE_CF: &str = "app_state_cf";

/// Column family for Kademlia store
pub const KADEMLIA_STORE_CF: &str = "kademlia_store_cf";

pub type RocksDBKey = (Option<&'static str>, Vec<u8>);

#[derive(Eq, Hash, PartialEq)]
pub struct HashMapKey(pub String);

/// Type of the database key which we can get from the custom key.
pub trait RecordKey: Into<RocksDBKey> + Into<HashMapKey> {
	type Type: Serialize + for<'a> Deserialize<'a> + Encode + Decode;
}

pub trait Database {
	/// Puts value for given key into database.
	/// Key is serialized into database key, value is serialized into type supported by database.
	fn put<T: RecordKey>(&self, key: T, value: T::Type) -> Result<()>;

	/// Gets value for given key.
	/// Key is serialized into database key, value is deserialized into the given type.
	fn get<T: RecordKey>(&self, key: T) -> Result<Option<T::Type>>;

	/// Deletes value from the database for the given key.
	fn delete<T: RecordKey>(&self, key: T) -> Result<()>;
}

#[derive(Clone)]
pub enum Key {
	AppData(u32, u32),
	BlockHeader(u32),
	VerifiedCellCount(u32),
	FinalitySyncCheckpoint,
	RpcNode,
	IsFinalitySynced,
	SyncDataVerified,
}

#[derive(Serialize, Deserialize, Debug, Decode, Encode)]
pub struct FinalitySyncCheckpoint {
	pub number: u32,
	pub set_id: u64,
	pub validator_set: Vec<ed25519::Public>,
}
