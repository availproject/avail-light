use codec::{Decode, Encode};
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};
use sp_core::ed25519;

pub mod keys;
pub mod rocks_db;

#[cfg(test)]
pub mod mem_db;

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

/// Column family for application state
pub const APP_STATE_CF: &str = "app_state_cf";

/// Column family for Kademlia store
pub const KADEMLIA_STORE_CF: &str = "kademlia_store_cf";

/// Keys predefined for persistance:
/// App ID prefix used with App Data key
const APP_ID_PREFIX: &str = "app_id";
/// Prefix used with current Block Header key
const BLOCK_HEADER_KEY_PREFIX: &str = "block_header";
/// Prefix used with Verified Cell Count key
const VERIFIED_CELL_COUNT_PREFIX: &str = "verified_cell_count";
/// Sync finality checkpoint key name
const FINALITY_SYNC_CHECKPOINT_KEY: &str = "finality_sync_checkpoint";
/// Finality Sync flag key
const IS_FINALITY_SYNCED_KEY: &str = "is_finality_sync";
/// Connected RPC Node details key
const CONNECTED_RPC_NODE_KEY: &str = "connected_rpc_node";
/// Key for Sync Data that has been verified
const SYNC_DATA_VERIFIED: &str = "sync_data_verified";

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
