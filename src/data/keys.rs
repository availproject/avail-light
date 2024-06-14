use avail_subxt::primitives::Header;

use crate::{network::rpc::Node as RpcNode, types::BlockRange};

use super::{FinalitySyncCheckpoint, HashMapKey, RecordKey, RocksDBKey, APP_STATE_CF};

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
const VERIFIED_SYNC_DATA: &str = "verified_sync_data";
/// Achieved Sync Confidence key
const ACHIEVED_SYNC_CONFIDENCE_KEY: &str = "achieved_sync_confidence";
/// Verified Sync Header key
const VERIFIED_SYNC_HEADER_KEY: &str = "verified_sync_header";
/// Key for storing Latest Sync
const LATEST_SYNC_KEY: &str = "latest_sync";
/// Key for storing Verified Data
const VERIFIED_DATA_KEY: &str = "verified_data";
/// Key for storing Achieved Confidence
const ACHIEVED_CONFIDENCE_KEY: &str = "achieved_confidence";
/// Key for storing Verified Header
const VERIFIED_HEADER_KEY: &str = "verified_header";
/// Key for storing Latest Header number
const LATEST_HEADER_KEY: &str = "latest_header";

pub struct AppDataKey(pub u32, pub u32);

impl RecordKey for AppDataKey {
	type Type = Vec<Vec<u8>>;
}

impl From<AppDataKey> for HashMapKey {
	fn from(value: AppDataKey) -> Self {
		let AppDataKey(app_id, block_num) = value;
		HashMapKey(format!(
			"{APP_STATE_CF}:{APP_ID_PREFIX}:{app_id}:{block_num}"
		))
	}
}

impl From<AppDataKey> for RocksDBKey {
	fn from(value: AppDataKey) -> Self {
		let AppDataKey(app_id, block_num) = value;
		(
			Some(APP_STATE_CF),
			format!("{APP_ID_PREFIX}:{app_id}:{block_num}").into_bytes(),
		)
	}
}

pub struct BlockHeaderKey(pub u32);

impl RecordKey for BlockHeaderKey {
	type Type = Header;
}

impl From<BlockHeaderKey> for HashMapKey {
	fn from(value: BlockHeaderKey) -> Self {
		let BlockHeaderKey(block_num) = value;
		HashMapKey(format!(
			"{APP_STATE_CF}:{BLOCK_HEADER_KEY_PREFIX}:{block_num}"
		))
	}
}

impl From<BlockHeaderKey> for RocksDBKey {
	fn from(value: BlockHeaderKey) -> Self {
		let BlockHeaderKey(block_num) = value;
		(
			Some(APP_STATE_CF),
			format!("{BLOCK_HEADER_KEY_PREFIX}:{block_num}").into_bytes(),
		)
	}
}

pub struct VerifiedCellCountKey(pub u32);

impl RecordKey for VerifiedCellCountKey {
	type Type = u32;
}

impl From<VerifiedCellCountKey> for HashMapKey {
	fn from(value: VerifiedCellCountKey) -> Self {
		let VerifiedCellCountKey(block_num) = value;
		HashMapKey(format!(
			"{APP_STATE_CF}:{VERIFIED_CELL_COUNT_PREFIX}:{block_num}"
		))
	}
}

impl From<VerifiedCellCountKey> for RocksDBKey {
	fn from(value: VerifiedCellCountKey) -> Self {
		let VerifiedCellCountKey(count) = value;
		(
			Some(APP_STATE_CF),
			format!("{VERIFIED_CELL_COUNT_PREFIX}:{count}").into_bytes(),
		)
	}
}

pub struct FinalitySyncCheckpointKey;

impl RecordKey for FinalitySyncCheckpointKey {
	type Type = FinalitySyncCheckpoint;
}

impl From<FinalitySyncCheckpointKey> for HashMapKey {
	fn from(_: FinalitySyncCheckpointKey) -> Self {
		HashMapKey(FINALITY_SYNC_CHECKPOINT_KEY.to_string())
	}
}

impl From<FinalitySyncCheckpointKey> for RocksDBKey {
	fn from(_: FinalitySyncCheckpointKey) -> Self {
		(
			Some(APP_STATE_CF),
			FINALITY_SYNC_CHECKPOINT_KEY.as_bytes().to_vec(),
		)
	}
}

pub struct RpcNodeKey;

impl RecordKey for RpcNodeKey {
	type Type = RpcNode;
}

impl From<RpcNodeKey> for HashMapKey {
	fn from(_: RpcNodeKey) -> Self {
		HashMapKey(CONNECTED_RPC_NODE_KEY.to_string())
	}
}

impl From<RpcNodeKey> for RocksDBKey {
	fn from(_: RpcNodeKey) -> Self {
		(
			Some(APP_STATE_CF),
			CONNECTED_RPC_NODE_KEY.as_bytes().to_vec(),
		)
	}
}

pub struct IsFinalitySyncedKey;

impl RecordKey for IsFinalitySyncedKey {
	type Type = bool;
}

impl From<IsFinalitySyncedKey> for HashMapKey {
	fn from(_: IsFinalitySyncedKey) -> Self {
		HashMapKey(IS_FINALITY_SYNCED_KEY.to_string())
	}
}

impl From<IsFinalitySyncedKey> for RocksDBKey {
	fn from(_: IsFinalitySyncedKey) -> Self {
		(
			Some(APP_STATE_CF),
			IS_FINALITY_SYNCED_KEY.as_bytes().to_vec(),
		)
	}
}

pub struct VerifiedSyncDataKey;

impl RecordKey for VerifiedSyncDataKey {
	type Type = Option<BlockRange>;
}

impl From<VerifiedSyncDataKey> for HashMapKey {
	fn from(_: VerifiedSyncDataKey) -> Self {
		HashMapKey(VERIFIED_SYNC_DATA.to_string())
	}
}

impl From<VerifiedSyncDataKey> for RocksDBKey {
	fn from(_: VerifiedSyncDataKey) -> Self {
		(Some(APP_STATE_CF), VERIFIED_SYNC_DATA.as_bytes().to_vec())
	}
}

pub struct AchievedSyncConfidenceKey;

impl RecordKey for AchievedSyncConfidenceKey {
	type Type = Option<BlockRange>;
}

impl From<AchievedSyncConfidenceKey> for HashMapKey {
	fn from(_: AchievedSyncConfidenceKey) -> Self {
		HashMapKey(ACHIEVED_SYNC_CONFIDENCE_KEY.to_string())
	}
}

impl From<AchievedSyncConfidenceKey> for RocksDBKey {
	fn from(_: AchievedSyncConfidenceKey) -> Self {
		(
			Some(APP_STATE_CF),
			ACHIEVED_SYNC_CONFIDENCE_KEY.as_bytes().to_vec(),
		)
	}
}

pub struct VerifiedSyncHeaderKey;

impl RecordKey for VerifiedSyncHeaderKey {
	type Type = Option<BlockRange>;
}

impl From<VerifiedSyncHeaderKey> for HashMapKey {
	fn from(_: VerifiedSyncHeaderKey) -> Self {
		HashMapKey(VERIFIED_SYNC_HEADER_KEY.to_string())
	}
}

impl From<VerifiedSyncHeaderKey> for RocksDBKey {
	fn from(_: VerifiedSyncHeaderKey) -> Self {
		(
			Some(APP_STATE_CF),
			VERIFIED_SYNC_HEADER_KEY.as_bytes().to_vec(),
		)
	}
}

pub struct LatestSyncKey;

impl RecordKey for LatestSyncKey {
	type Type = Option<u32>;
}

impl From<LatestSyncKey> for HashMapKey {
	fn from(_: LatestSyncKey) -> Self {
		HashMapKey(LATEST_SYNC_KEY.to_string())
	}
}

impl From<LatestSyncKey> for RocksDBKey {
	fn from(_: LatestSyncKey) -> Self {
		(Some(APP_STATE_CF), LATEST_SYNC_KEY.as_bytes().to_vec())
	}
}

pub struct VerifiedDataKey;

impl RecordKey for VerifiedDataKey {
	type Type = Option<BlockRange>;
}

impl From<VerifiedDataKey> for HashMapKey {
	fn from(_: VerifiedDataKey) -> Self {
		HashMapKey(VERIFIED_DATA_KEY.to_string())
	}
}

impl From<VerifiedDataKey> for RocksDBKey {
	fn from(_: VerifiedDataKey) -> Self {
		(Some(APP_STATE_CF), VERIFIED_DATA_KEY.as_bytes().to_vec())
	}
}

pub struct AchievedConfidenceKey;

impl RecordKey for AchievedConfidenceKey {
	type Type = Option<BlockRange>;
}

impl From<AchievedConfidenceKey> for HashMapKey {
	fn from(_: AchievedConfidenceKey) -> Self {
		HashMapKey(ACHIEVED_CONFIDENCE_KEY.to_string())
	}
}

impl From<AchievedConfidenceKey> for RocksDBKey {
	fn from(_: AchievedConfidenceKey) -> Self {
		(
			Some(APP_STATE_CF),
			ACHIEVED_CONFIDENCE_KEY.as_bytes().to_vec(),
		)
	}
}

pub struct VerifiedHeaderKey;

impl RecordKey for VerifiedHeaderKey {
	type Type = Option<BlockRange>;
}

impl From<VerifiedHeaderKey> for HashMapKey {
	fn from(_: VerifiedHeaderKey) -> Self {
		HashMapKey(VERIFIED_HEADER_KEY.to_string())
	}
}

impl From<VerifiedHeaderKey> for RocksDBKey {
	fn from(_: VerifiedHeaderKey) -> Self {
		(Some(APP_STATE_CF), VERIFIED_HEADER_KEY.as_bytes().to_vec())
	}
}

pub struct LatestHeaderKey;

impl RecordKey for LatestHeaderKey {
	type Type = u32;
}

impl From<LatestHeaderKey> for HashMapKey {
	fn from(_: LatestHeaderKey) -> Self {
		HashMapKey(LATEST_HEADER_KEY.to_string())
	}
}

impl From<LatestHeaderKey> for RocksDBKey {
	fn from(_: LatestHeaderKey) -> Self {
		(Some(APP_STATE_CF), LATEST_HEADER_KEY.as_bytes().to_vec())
	}
}
