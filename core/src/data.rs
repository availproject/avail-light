use std::fmt::Display;

use self::rocks_db::RocksDBKey;
use crate::{network::rpc::Node as RpcNode, types::BlockRange};
use avail_subxt::primitives::Header;
use codec::{Decode, Encode, Error, Input};
#[cfg(test)]
use mem_db::HashMapKey;
use serde::{Deserialize, Serialize};
use sp_core::ed25519;

mod keys;
#[cfg(test)]
mod mem_db;
mod rocks_db;

#[cfg(test)]
pub use mem_db::MemoryDB;
pub use rocks_db::RocksDB;

/// Column family for application state
pub const APP_STATE_CF: &str = "app_state_cf";

/// Column family for Kademlia store
pub const KADEMLIA_STORE_CF: &str = "kademlia_store_cf";

#[cfg(not(test))]
/// Type of the database key which we can get from the custom key.
pub trait RecordKey: Into<RocksDBKey> {
	type Type: Serialize + for<'a> Deserialize<'a> + Encode + Decode;
}

#[cfg(test)]
/// Type of the database key which we can get from the custom key.
pub trait RecordKey: Into<RocksDBKey> + Into<HashMapKey> {
	type Type: Serialize + for<'a> Deserialize<'a> + Encode + Decode;
}

pub trait Database {
	/// Puts value for given key into database.
	/// Key is serialized into database key, value is serialized into type supported by database.
	fn put<T: RecordKey>(&self, key: T, value: T::Type);

	/// Gets value for given key.
	/// Key is serialized into database key, value is deserialized into the given type.
	fn get<T: RecordKey>(&self, key: T) -> Option<T::Type>;

	/// Deletes value from the database for the given key.
	fn delete<T: RecordKey>(&self, key: T);
}

#[derive(Serialize, Deserialize, Debug, Decode, Encode)]
pub struct FinalitySyncCheckpoint {
	pub number: u32,
	pub set_id: u64,
	pub validator_set: Vec<ed25519::Public>,
}

pub struct AppDataKey(pub u32, pub u32);

impl RecordKey for AppDataKey {
	type Type = Vec<Vec<u8>>;
}

pub struct BlockHeaderKey(pub u32);

impl RecordKey for BlockHeaderKey {
	type Type = Header;
}

pub struct VerifiedCellCountKey(pub u32);

impl RecordKey for VerifiedCellCountKey {
	type Type = u32;
}

pub struct FinalitySyncCheckpointKey;

impl RecordKey for FinalitySyncCheckpointKey {
	type Type = FinalitySyncCheckpoint;
}

pub struct RpcNodeKey;

impl RecordKey for RpcNodeKey {
	type Type = RpcNode;
}

pub struct IsFinalitySyncedKey;

impl RecordKey for IsFinalitySyncedKey {
	type Type = bool;
}

#[derive(Clone)]
pub struct VerifiedSyncDataKey;

impl RecordKey for VerifiedSyncDataKey {
	type Type = BlockRange;
}

pub struct AchievedSyncConfidenceKey;

impl RecordKey for AchievedSyncConfidenceKey {
	type Type = BlockRange;
}
pub struct VerifiedSyncHeaderKey;

impl RecordKey for VerifiedSyncHeaderKey {
	type Type = BlockRange;
}

pub struct LatestSyncKey;

impl RecordKey for LatestSyncKey {
	type Type = u32;
}

#[derive(Clone)]
pub struct VerifiedDataKey;

impl RecordKey for VerifiedDataKey {
	type Type = BlockRange;
}

pub struct AchievedConfidenceKey;

impl RecordKey for AchievedConfidenceKey {
	type Type = BlockRange;
}

pub struct VerifiedHeaderKey;

impl RecordKey for VerifiedHeaderKey {
	type Type = BlockRange;
}

pub struct LatestHeaderKey;

impl RecordKey for LatestHeaderKey {
	type Type = u32;
}

pub struct IsSyncedKey;

impl RecordKey for IsSyncedKey {
	type Type = bool;
}

pub struct ClientIdKey;

impl RecordKey for ClientIdKey {
	type Type = Uuid;
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Uuid(uuid::Uuid);

impl Uuid {
	pub fn new_v4() -> Uuid {
		Self(uuid::Uuid::new_v4())
	}
}

impl Display for Uuid {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.0.to_string())
	}
}

impl Decode for Uuid {
	fn decode<I: Input>(input: &mut I) -> Result<Self, Error> {
		let mut bytes = [0u8; 16];
		input.read(&mut bytes)?;
		uuid::Uuid::from_slice(&bytes)
			.map_err(|_| Error::from("failed to decode uuid"))
			.map(Uuid)
	}
}

impl Encode for Uuid {
	fn encode(&self) -> Vec<u8> {
		self.0.as_bytes().to_vec()
	}
}
