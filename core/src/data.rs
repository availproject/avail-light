use crate::{
	network::rpc::Node as RpcNode,
	types::{BlockRange, Uuid},
};
use avail_rust::AvailHeader;
use codec::{Decode, Encode};
use serde::{Deserialize, Serialize};
use sp_core::ed25519;

mod keys;
use keys::*;

mod mem_db;
pub use mem_db::*;

#[cfg(feature = "rocksdb")]
mod rocks_db;

#[cfg(not(feature = "rocksdb"))]
pub type DB = mem_db::MemoryDB;
#[cfg(feature = "rocksdb")]
pub type DB = rocks_db::RocksDB;

/// Column family for application state
pub const APP_STATE_CF: &str = "app_state_cf";

/// Column family for Kademlia store
pub const KADEMLIA_STORE_CF: &str = "kademlia_store_cf";

/// Type of the database key which we can get from the custom key.
pub trait RecordKey {
	type Type: Serialize + for<'a> Deserialize<'a> + Encode + Decode;

	fn space(&self) -> Option<&'static str>;

	/// Returns key space (if any) and key
	fn key(&self) -> String;
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

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		let AppDataKey(app_id, block_num) = self;
		format!("{APP_ID_PREFIX}:{app_id}:{block_num}")
	}
}

pub struct BlockHeaderKey(pub u32);

impl RecordKey for BlockHeaderKey {
	type Type = AvailHeader;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		let BlockHeaderKey(block_num) = self;
		format!("{BLOCK_HEADER_KEY_PREFIX}:{block_num}")
	}
}

pub struct VerifiedCellCountKey(pub u32);

impl RecordKey for VerifiedCellCountKey {
	type Type = u32;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		let VerifiedCellCountKey(count) = self;
		format!("{VERIFIED_CELL_COUNT_PREFIX}:{count}")
	}
}

pub struct FinalitySyncCheckpointKey;

impl RecordKey for FinalitySyncCheckpointKey {
	type Type = FinalitySyncCheckpoint;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		FINALITY_SYNC_CHECKPOINT_KEY.into()
	}
}

pub struct RpcNodeKey;

impl RecordKey for RpcNodeKey {
	type Type = RpcNode;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		CONNECTED_RPC_NODE_KEY.into()
	}
}

pub struct IsFinalitySyncedKey;

impl RecordKey for IsFinalitySyncedKey {
	type Type = bool;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		IS_FINALITY_SYNCED_KEY.into()
	}
}

#[derive(Clone)]
pub struct VerifiedSyncDataKey;

impl RecordKey for VerifiedSyncDataKey {
	type Type = BlockRange;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		VERIFIED_SYNC_DATA.into()
	}
}

pub struct AchievedSyncConfidenceKey;

impl RecordKey for AchievedSyncConfidenceKey {
	type Type = BlockRange;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		ACHIEVED_SYNC_CONFIDENCE_KEY.into()
	}
}
pub struct VerifiedSyncHeaderKey;

impl RecordKey for VerifiedSyncHeaderKey {
	type Type = BlockRange;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		VERIFIED_SYNC_HEADER_KEY.into()
	}
}

pub struct LatestSyncKey;

impl RecordKey for LatestSyncKey {
	type Type = u32;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		LATEST_SYNC_KEY.into()
	}
}

#[derive(Clone)]
pub struct VerifiedDataKey;

impl RecordKey for VerifiedDataKey {
	type Type = BlockRange;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		VERIFIED_DATA_KEY.into()
	}
}

pub struct AchievedConfidenceKey;

impl RecordKey for AchievedConfidenceKey {
	type Type = BlockRange;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		ACHIEVED_CONFIDENCE_KEY.into()
	}
}

pub struct VerifiedHeaderKey;

impl RecordKey for VerifiedHeaderKey {
	type Type = BlockRange;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		VERIFIED_HEADER_KEY.into()
	}
}

pub struct LatestHeaderKey;

impl RecordKey for LatestHeaderKey {
	type Type = u32;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		LATEST_HEADER_KEY.into()
	}
}

pub struct IsSyncedKey;

impl RecordKey for IsSyncedKey {
	type Type = bool;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		IS_SYNCED_KEY.into()
	}
}

pub struct ClientIdKey;

impl RecordKey for ClientIdKey {
	type Type = Uuid;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		CLIENT_ID_KEY.into()
	}
}

pub struct P2PKeypairKey;

impl RecordKey for P2PKeypairKey {
	type Type = Vec<u8>;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		P2P_KEYPAIR_KEY.into()
	}
}

pub struct SignerNonceKey;

impl RecordKey for SignerNonceKey {
	type Type = u32;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		SIGNER_NONCE.into()
	}
}

pub struct BlockHeaderReceivedAtKey(pub u32);

impl RecordKey for BlockHeaderReceivedAtKey {
	type Type = u64;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		let BlockHeaderReceivedAtKey(block_num) = self;
		format!("{BLOCK_HEADER_RECEIVED_AT}:{block_num}")
	}
}

pub struct PeerIDKey;

impl RecordKey for PeerIDKey {
	type Type = String;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		PEER_ID_KEY.into()
	}
}

pub struct MultiAddressKey;

impl RecordKey for MultiAddressKey {
	type Type = String;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		MULTI_ADDRESS_KEY.into()
	}
}

pub struct DhtFetchedPercentageKey;

impl RecordKey for DhtFetchedPercentageKey {
	type Type = f64;

	fn space(&self) -> Option<&'static str> {
		Some(APP_STATE_CF)
	}

	fn key(&self) -> String {
		DHT_FETCHED_PERCENTAGE.into()
	}
}