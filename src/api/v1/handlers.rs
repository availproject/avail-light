use super::types::{AppDataQuery, ClientResponse, ConfidenceResponse, LatestBlockResponse, Status};
use crate::{
	api::v1::types::{Extrinsics, ExtrinsicsDataResponse},
	data::{Database, Key},
	types::{Mode, OptionBlockRange, State},
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
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

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
	state: Arc<Mutex<State>>,
) -> ClientResponse<ConfidenceResponse> {
	info!("Got request for confidence for block {block_num}");
	let res = match db.get(Key::VerifiedCellCount(block_num)) {
		Ok(Some(count)) => {
			let confidence = calculate_confidence(count);
			let serialised_confidence = serialised_confidence(block_num, confidence);
			ClientResponse::Normal(ConfidenceResponse {
				block: block_num,
				confidence,
				serialised_confidence,
			})
		},
		Ok(None) => {
			let state = state.lock().unwrap();
			if state
				.confidence_achieved
				.as_ref()
				.map(|range| block_num < range.last)
				.unwrap_or(false)
			{
				return ClientResponse::NotFinalized;
			} else {
				return ClientResponse::NotFound;
			}
		},
		Err(e) => ClientResponse::Error(e),
	};
	info!("Returning confidence: {res:?}");
	res
}

pub fn status(
	app_id: Option<u32>,
	state: Arc<Mutex<State>>,
	db: impl Database,
) -> ClientResponse<Status> {
	let state = state.lock().unwrap();
	let Some(last) = state.confidence_achieved.last() else {
		return ClientResponse::NotFound;
	};
	let res = match db.get(Key::VerifiedCellCount(last)) {
		Ok(Some(count)) => {
			let confidence = calculate_confidence(count);
			ClientResponse::Normal(Status {
				block_num: last,
				confidence,
				app_id,
			})
		},
		Ok(None) => ClientResponse::NotFound,

		Err(e) => ClientResponse::Error(e),
	};
	info!("Returning status: {res:?}");
	res
}

pub fn latest_block(state: Arc<Mutex<State>>) -> ClientResponse<LatestBlockResponse> {
	info!("Got request for latest block");
	let state = state.lock().unwrap();
	match state.confidence_achieved.last() {
		None => ClientResponse::NotFound,
		Some(latest_block) => ClientResponse::Normal(LatestBlockResponse { latest_block }),
	}
}

pub fn appdata(
	block_num: u32,
	query: AppDataQuery,
	db: impl Database,
	app_id: Option<u32>,
	state: Arc<Mutex<State>>,
) -> ClientResponse<ExtrinsicsDataResponse> {
	fn decode_app_data_to_extrinsics(
		data: Result<Option<Vec<Vec<u8>>>>,
	) -> Result<Option<Vec<AppUncheckedExtrinsic>>> {
		let xts = data.map(|e| {
			e.map(|e| {
				e.iter()
					.enumerate()
					.map(|(i, raw)| {
						<_ as Decode>::decode(&mut &raw[..])
							.wrap_err(format!("Couldn't decode AvailExtrinsic num {i}"))
					})
					.collect::<Result<Vec<_>>>()
			})
		});
		match xts {
			Ok(Some(Ok(s))) => Ok(Some(s)),
			Ok(Some(Err(e))) => Err(e),
			Ok(None) => Ok(None),
			Err(e) => Err(e),
		}
	}
	info!("Got request for AppData for block {block_num}");
	let state = state.lock().unwrap();
	let last = state.confidence_achieved.last();
	let decode = query.decode.unwrap_or(false);
	let res = match decode_app_data_to_extrinsics(
		db.get(Key::AppData(app_id.unwrap_or(0u32), block_num)),
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

		Ok(None) => match last {
			Some(last) if block_num == last => ClientResponse::InProcess,
			_ => ClientResponse::NotFound,
		},
		Err(e) => ClientResponse::Error(e),
	};
	debug!("Returning AppData: {res:?}");
	res
}
