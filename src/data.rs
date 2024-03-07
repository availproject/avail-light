use codec::{Decode, Encode};
use serde::{Deserialize, Serialize};
use sp_core::ed25519;

use crate::db::data::mem::HashMapKey;

// pub mod database;
// pub mod rocks_db;

/// Column family for confidence factor
pub const CONFIDENCE_FACTOR_CF: &str = "avail_light_confidence_factor_cf";

/// Column family for block header
pub const BLOCK_HEADER_CF: &str = "avail_light_block_header_cf";

/// Column family for app data
pub const APP_DATA_CF: &str = "avail_light_app_data_cf";

/// Column family for state
pub const STATE_CF: &str = "avail_light_state_cf";

/// Sync finality checkpoint key name
const FINALITY_SYNC_CHECKPOINT_KEY: &str = "finality_sync_checkpoint";

#[derive(Clone)]
pub enum Key {
	AppData(u32, u32),
	BlockHeader(u32),
	ConfidenceFactor(u32),
	FinalitySyncCheckpoint,
}

impl From<Key> for (Option<&'static str>, Vec<u8>) {
	fn from(key: Key) -> Self {
		match key {
			Key::AppData(app_id, block_number) => (
				Some(APP_DATA_CF),
				format!("{app_id}:{block_number}").into_bytes(),
			),
			Key::BlockHeader(block_number) => {
				(Some(BLOCK_HEADER_CF), block_number.to_be_bytes().to_vec())
			},
			Key::ConfidenceFactor(block_number) => (
				Some(CONFIDENCE_FACTOR_CF),
				block_number.to_be_bytes().to_vec(),
			),
			Key::FinalitySyncCheckpoint => (
				Some(STATE_CF),
				FINALITY_SYNC_CHECKPOINT_KEY.as_bytes().to_vec(),
			),
		}
	}
}

impl From<Key> for HashMapKey {
	fn from(key: Key) -> Self {
		match key {
			Key::AppData(app_id, block_number) => {
				HashMapKey(format!("{APP_DATA_CF}:{app_id}:{block_number}"))
			},
			Key::BlockHeader(block_number) => {
				HashMapKey(format!("{BLOCK_HEADER_CF}:{block_number}"))
			},
			Key::ConfidenceFactor(block_number) => {
				HashMapKey(format!("{CONFIDENCE_FACTOR_CF}:{block_number}"))
			},
			Key::FinalitySyncCheckpoint => HashMapKey(format!("{FINALITY_SYNC_CHECKPOINT_KEY}")),
		}
	}
}

#[derive(Serialize, Deserialize, Debug, Decode, Encode)]
pub struct FinalitySyncCheckpoint {
	pub number: u32,
	pub set_id: u64,
	pub validator_set: Vec<ed25519::Public>,
}
