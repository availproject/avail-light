use super::types::{AppDataQuery, ClientResponse, ConfidenceResponse, LatestBlockResponse, Status};
use crate::{
	api::v1::types::{Extrinsics, ExtrinsicsDataResponse},
	data::{AchievedConfidenceKey, AppDataKey, Database, VerifiedCellCountKey},
	network::rpc::cell_count_for_confidence,
	types::{BlockRange, Mode, RuntimeConfig},
	utils::calculate_confidence,
};
use avail_subxt::{
	api::runtime_types::{da_control::pallet::Call, da_runtime::RuntimeCall},
	primitives::AppUncheckedExtrinsic,
};
use base64::{engine::general_purpose, Engine};
use codec::Decode;
use color_eyre::{eyre::WrapErr, Result};
use num::{BigUint, FromPrimitive};
use tracing::{debug, info};

fn get_achived_confidence(db: &impl Database) -> Option<BlockRange> {
	db.get(AchievedConfidenceKey)
}

fn serialised_confidence(block: u32, factor: f64) -> Option<String> {
	let block_big: BigUint = FromPrimitive::from_u64(block as u64)?;
	let factor_big: BigUint = FromPrimitive::from_u64((10f64.powi(7) * factor) as u64)?;
	let shifted: BigUint = block_big << 32 | factor_big;
	Some(shifted.to_str_radix(10))
}

pub fn mode(app_id: Option<u32>) -> ClientResponse<Mode> {
	ClientResponse::Normal(Mode::from(app_id))
}

pub fn confidence(
	block_num: u32,
	db: impl Database,
	cfg: RuntimeConfig,
) -> ClientResponse<ConfidenceResponse> {
	fn is_synced(block_num: u32, db: impl Database) -> bool {
		get_achived_confidence(&db).map_or(false, |range| block_num <= range.last)
	}

	info!("Got request for confidence for block {block_num}");

	let count = match db.get(VerifiedCellCountKey(block_num)) {
		Some(count) => count,
		None if is_synced(block_num, db) => cell_count_for_confidence(cfg.confidence),
		None => return ClientResponse::NotFinalized,
	};

	let confidence = calculate_confidence(count);
	let serialised_confidence = serialised_confidence(block_num, confidence);

	let response = ClientResponse::Normal(ConfidenceResponse {
		block: block_num,
		confidence,
		serialised_confidence,
	});
	info!("Returning confidence: {response:?}");
	response
}

pub fn status(app_id: Option<u32>, db: impl Database) -> ClientResponse<Status> {
	let Some(BlockRange { last, .. }) = get_achived_confidence(&db) else {
		return ClientResponse::NotFound;
	};
	let res = match db.get(VerifiedCellCountKey(last)) {
		Some(count) => {
			let confidence = calculate_confidence(count);
			ClientResponse::Normal(Status {
				block_num: last,
				confidence,
				app_id,
			})
		},
		None => ClientResponse::NotFound,
	};
	info!("Returning status: {res:?}");
	res
}

pub fn latest_block(db: impl Database) -> ClientResponse<LatestBlockResponse> {
	info!("Got request for latest block");
	let Some(BlockRange { last, .. }) = get_achived_confidence(&db) else {
		return ClientResponse::NotFound;
	};

	ClientResponse::Normal(LatestBlockResponse { latest_block: last })
}

pub fn appdata(
	block_num: u32,
	query: AppDataQuery,
	db: impl Database,
	app_id: Option<u32>,
) -> ClientResponse<ExtrinsicsDataResponse> {
	fn decode_app_data_to_extrinsics(
		data: Option<Vec<Vec<u8>>>,
	) -> Result<Option<Vec<AppUncheckedExtrinsic>>> {
		let xts = data.map(|e| {
			e.iter()
				.enumerate()
				.map(|(i, raw)| {
					<_ as Decode>::decode(&mut &raw[..])
						.wrap_err(format!("Couldn't decode AvailExtrinsic num {i}"))
				})
				.collect::<Result<Vec<_>>>()
		});
		match xts {
			Some(Ok(s)) => Ok(Some(s)),
			Some(Err(e)) => Err(e),
			None => Ok(None),
		}
	}
	info!("Got request for AppData for block {block_num}");
	let decode = query.decode.unwrap_or(false);
	let res = match decode_app_data_to_extrinsics(
		db.get(AppDataKey(app_id.unwrap_or(0u32), block_num)),
	) {
		Ok(Some(data)) => {
			if !decode {
				ClientResponse::Normal(ExtrinsicsDataResponse {
					block: block_num,
					extrinsics: Extrinsics::Encoded(data),
				})
			} else {
				let xts = data
					.iter()
					.flat_map(|xt| match &xt.function {
						RuntimeCall::DataAvailability(Call::submit_data { data, .. }) => Some(data),
						_ => None,
					})
					.map(|data| general_purpose::STANDARD.encode(data.0.as_slice()))
					.collect::<Vec<_>>();
				ClientResponse::Normal(ExtrinsicsDataResponse {
					block: block_num,
					extrinsics: Extrinsics::Decoded(xts),
				})
			}
		},

		Ok(None) => get_achived_confidence(&db)
			.filter(|range| block_num == range.last)
			.map_or(ClientResponse::NotFound, |_| ClientResponse::InProcess),
		Err(e) => ClientResponse::Error(e),
	};
	debug!("Returning AppData: {res:?}");
	res
}
