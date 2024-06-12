use avail_subxt::primitives::Header;

use crate::{network::rpc::Node as RpcNode, types::BlockRange};

use super::{
	FinalitySyncCheckpoint, HashMapKey, RecordKey, RocksDBKey, APP_ID_PREFIX, APP_STATE_CF,
	BLOCK_HEADER_KEY_PREFIX, CONNECTED_RPC_NODE_KEY, FINALITY_SYNC_CHECKPOINT_KEY,
	IS_FINALITY_SYNCED_KEY, SYNC_DATA_VERIFIED, VERIFIED_CELL_COUNT_PREFIX,
};

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

pub struct SyncDataVerifiedKey;

impl RecordKey for SyncDataVerifiedKey {
	type Type = Option<BlockRange>;
}

impl From<SyncDataVerifiedKey> for HashMapKey {
	fn from(_: SyncDataVerifiedKey) -> Self {
		HashMapKey(SYNC_DATA_VERIFIED.to_string())
	}
}

impl From<SyncDataVerifiedKey> for RocksDBKey {
	fn from(_: SyncDataVerifiedKey) -> Self {
		(Some(APP_STATE_CF), SYNC_DATA_VERIFIED.as_bytes().to_vec())
	}
}
