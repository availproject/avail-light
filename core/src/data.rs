use crate::{
	network::rpc::Node as RpcNode,
	types::{BlockRange, Uuid},
};
use avail_subxt::primitives::Header;
use codec::{Decode, Encode};
use serde::{Deserialize, Serialize};
use sp_core::ed25519;

mod keys;

#[cfg(not(feature = "rocksdb"))]
mod mem_db;
#[cfg(not(feature = "rocksdb"))]
pub use mem_db::*;

#[cfg(feature = "rocksdb")]
mod rocks_db;
#[cfg(feature = "rocksdb")]
pub use rocks_db::*;

/// Column family for application state
pub const APP_STATE_CF: &str = "app_state_cf";

/// Column family for Kademlia store
pub const KADEMLIA_STORE_CF: &str = "kademlia_store_cf";

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

pub struct P2PKeypairKey;

impl RecordKey for P2PKeypairKey {
	type Type = Vec<u8>;
}
