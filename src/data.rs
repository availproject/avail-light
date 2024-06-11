use codec::{Decode, Encode};
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};
use sp_core::ed25519;

pub mod rocks_db;

#[cfg(test)]
pub mod mem_db;

pub trait Database {
	/// Type of the database key which we can get from the custom key.
	type Key;

	/// Puts value for given key into database.
	/// Key is serialized into database key, value is serialized into type supported by database.
	fn put<T>(&self, key: Key, value: T) -> Result<()>
	where
		T: Serialize + Encode;

	/// Gets value for given key.
	/// Key is serialized into database key, value is deserialized into the given type.
	fn get<T>(&self, key: Key) -> Result<Option<T>>
	where
		for<'a> T: Deserialize<'a> + Decode;

	/// Deletes value from the database for the given key.
	fn delete(&self, key: Key) -> Result<()>;
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
/// Connected RPC Node details key
const CONNECTED_RPC_NODE_KEY: &str = "connected_rpc_node";

#[derive(Clone)]
pub enum Key {
	AppData(u32, u32),
	BlockHeader(u32),
	VerifiedCellCount(u32),
	FinalitySyncCheckpoint,
	RpcNode,
}

#[derive(Serialize, Deserialize, Debug, Decode, Encode)]
pub struct FinalitySyncCheckpoint {
	pub number: u32,
	pub set_id: u64,
	pub validator_set: Vec<ed25519::Public>,
}
