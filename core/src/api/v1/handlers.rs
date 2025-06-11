use super::types::{AppDataQuery, ClientResponse, ConfidenceResponse, LatestBlockResponse, Status};
use crate::{
	api::{
		configuration::SharedConfig,
		v1::types::{Extrinsics, ExtrinsicsDataResponse},
	},
	data::{AchievedConfidenceKey, AppDataKey, Database, VerifiedCellCountKey},
	network::rpc::cell_count_for_confidence,
	types::{BlockRange, Mode},
	utils::calculate_confidence,
};
use avail_rust_client::avail_rust_core::{
	avail::data_availability::tx::Call, avail::RuntimeCall, OpaqueTransaction,
};
use base64::{engine::general_purpose, Engine};
use color_eyre::eyre::WrapErr;
use num::{BigUint, FromPrimitive};
use tracing::{debug, info};

fn get_achived_confidence(db: &impl Database) -> Option<BlockRange> {
	db.get(AchievedConfidenceKey)
}

fn serialised_confidence(block: u32, factor: f64) -> Option<String> {
	let block_big: BigUint = FromPrimitive::from_u64(block as u64)?;
	let factor_big: BigUint = FromPrimitive::from_u64((10f64.powi(7) * factor) as u64)?;
	let shifted: BigUint = (block_big << 32) | factor_big;
	Some(shifted.to_str_radix(10))
}

pub fn mode(app_id: Option<u32>) -> ClientResponse<Mode> {
	ClientResponse::Normal(Mode::from(app_id))
}

pub fn confidence(
	block_num: u32,
	db: impl Database,
	cfg: SharedConfig,
) -> ClientResponse<ConfidenceResponse> {
	fn is_synced(block_num: u32, db: impl Database) -> bool {
		get_achived_confidence(&db).is_some_and(|range| block_num <= range.last)
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
	info!("Got request for AppData for block {block_num}");
	let app_id = app_id.unwrap_or(0u32);
	let raw_exts = db.get(AppDataKey(app_id, block_num));
	let Some(raw_exts) = raw_exts else {
		let res = get_achived_confidence(&db)
			.filter(|range| block_num == range.last)
			.map_or(ClientResponse::NotFound, |_| ClientResponse::InProcess);
		debug!("Returning AppData: {res:?}");
		return res;
	};

	let decode = query.decode.unwrap_or(false);
	let mut decoded: Vec<String> = Vec::with_capacity(raw_exts.len());
	let mut encoded: Vec<Vec<u8>> = Vec::with_capacity(raw_exts.len());
	for (i, raw_ext) in raw_exts.into_iter().enumerate() {
		let opaque = match OpaqueTransaction::try_from(&raw_ext)
			.wrap_err(format!("Couldn't decode AvailExtrinsic num {i}"))
		{
			Ok(ok) => ok,
			Err(report) => {
				let res = ClientResponse::Error(report);
				debug!("Returning AppData: {res:?}");
				return res;
			},
		};

		if decode {
			let Ok(RuntimeCall::DataAvailability(Call::SubmitData(data))) =
				RuntimeCall::try_from(&opaque.call)
			else {
				continue;
			};
			let data = general_purpose::STANDARD.encode(data.data.as_slice());
			decoded.push(data);
		} else {
			encoded.push(raw_ext);
		}
	}

	let res = if decode {
		ClientResponse::Normal(ExtrinsicsDataResponse {
			block: block_num,
			extrinsics: Extrinsics::Decoded(decoded),
		})
	} else {
		ClientResponse::Normal(ExtrinsicsDataResponse {
			block: block_num,
			extrinsics: Extrinsics::Encoded(encoded),
		})
	};

	debug!("Returning AppData: {res:?}");
	res
}
