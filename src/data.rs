use avail_subxt::Header;
use codec::{Decode, Encode};
use color_eyre::eyre::{eyre, Context, Ok, Result};
use kate_recovery::com::AppData;
use num::traits::ToBytes;
use sp_core::ed25519;

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

#[derive(Debug, Decode, Encode)]
pub struct FinalitySyncCheckpoint {
	pub number: u32,
	pub set_id: u64,
	pub validator_set: Vec<ed25519::Public>,
}

impl crate::db::data::Encode<Vec<u8>> for FinalitySyncCheckpoint {
	fn encode(&self) -> Result<Vec<u8>> {
		Ok(<Self as codec::Encode>::encode(&self))
	}
}

impl crate::db::data::Decode<Vec<u8>> for FinalitySyncCheckpoint {
	fn decode(source: Vec<u8>) -> Result<Self> {
		<Self as codec::Decode>::decode(&mut &source[..])
			.wrap_err("Decoding Finality Sync Checkpoint for RocksDB failed")
	}
}

impl crate::db::data::Encode<Vec<u8>> for AppData {
	fn encode(&self) -> Result<Vec<u8>> {
		Ok(<&Self as codec::Encode>::encode(&self))
	}
}

impl crate::db::data::Decode<Vec<u8>> for AppData {
	fn decode(source: Vec<u8>) -> Result<Self> {
		<Self as codec::Decode>::decode(&mut &source[..])
			.wrap_err("Decoding AppData for RocksDB failed")
	}
}

impl crate::db::data::Encode<Vec<u8>> for u32 {
	fn encode(&self) -> Result<Vec<u8>> {
		Ok(self.to_be_bytes().to_vec())
	}
}

impl crate::db::data::Decode<Vec<u8>> for u32 {
	fn decode(source: Vec<u8>) -> Result<Self> {
		source
			.try_into()
			.map_err(|_| eyre!("conversion failed"))
			.wrap_err("Unable to convert confidence (wrong number or bytes)")
			.map(u32::from_be_bytes)
	}
}

impl crate::db::data::Encode<Vec<u8>> for Header {
	fn encode(&self) -> Result<Vec<u8>> {
		let string = serde_json::to_string(&self)
			.wrap_err("Serializing Header failed for RocksDB failed")?;

		Ok(string.as_bytes().to_vec())
	}
}

impl crate::db::data::Decode<Vec<u8>> for Header {
	fn decode(source: Vec<u8>) -> Result<Self> {
		serde_json::from_slice(&source).wrap_err("Deserializing Header failed for RocksDB failed")
	}
}
