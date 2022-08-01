use std::{
	net::SocketAddr,
	str::FromStr,
	sync::{mpsc::SyncSender, Arc, Mutex},
};

use anyhow::{Context, Result};
use codec::Decode;
use num::{BigUint, FromPrimitive};
use rocksdb::DB;
use serde::{Deserialize, Serialize};
use tracing::info;
use warp::{http::StatusCode, Filter};

use crate::{
	app_client::AvailExtrinsic,
	data::{get_confidence_from_db, get_decoded_data_from_db},
	types::{CellContentQueryPayload, Mode, RuntimeConfig},
};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConfidenceResponse {
	pub block: u64,
	pub confidence: f64,
	pub serialised_confidence: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExtrinsicsDataResponse {
	pub block: u64,
	pub extrinsics: Vec<AvailExtrinsic>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LatestBlockResponse {
	pub latest_block: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Status {
	pub block_num: u64,
	confidence: f64,
	pub app_id: Option<u32>,
}
pub fn calculate_confidence(count: u32) -> f64 { 100f64 * (1f64 - 1f64 / 2u32.pow(count) as f64) }

pub fn serialised_confidence(block: u64, factor: f64) -> Option<String> {
	let block_big: BigUint = FromPrimitive::from_u64(block)?;
	let factor_big: BigUint = FromPrimitive::from_u64((10f64.powi(7) * factor) as u64)?;
	let shifted: BigUint = block_big << 32 | factor_big;
	Some(shifted.to_str_radix(10))
}

#[derive(Debug)]
enum ClientResponse<T>
where
	T: Serialize,
{
	Normal(T),
	NotFound,
	NotFinalized,
	InProcess,
	Error(anyhow::Error),
}

fn confidence(block_num: u64, db: Arc<DB>, counter: u64) -> ClientResponse<ConfidenceResponse> {
	info!("Got request for confidence for block {block_num}");
	let res = match get_confidence_from_db(db, block_num) {
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
			// let lock = counter.lock().unwrap();
			if block_num < counter {
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

fn status(cfg: &RuntimeConfig, counter: u64, db: Arc<DB>) -> ClientResponse<Status> {
	let res = match get_confidence_from_db(db, counter) {
		Ok(Some(count)) => {
			let confidence = calculate_confidence(count);
			ClientResponse::Normal(Status {
				block_num: counter,
				confidence,
				app_id: cfg.app_id,
			})
		},
		Ok(None) => ClientResponse::NotFound,

		Err(e) => ClientResponse::Error(e),
	};
	info!("Returning status: {res:?}");
	res
}

fn latest_block(counter: Arc<Mutex<u64>>) -> ClientResponse<LatestBlockResponse> {
	info!("Got request for latest block");
	let res = match counter.lock() {
		Ok(counter) => ClientResponse::Normal(LatestBlockResponse {
			latest_block: *counter,
		}),
		Err(_) => ClientResponse::NotFound,
	};
	res
}

fn appdata(
	block_num: u64,
	db: Arc<DB>,
	cfg: RuntimeConfig,
	counter: u64,
) -> ClientResponse<ExtrinsicsDataResponse> {
	if block_num < counter {
		return ClientResponse::NotFinalized;
	}
	fn decode_app_data_to_extrinsics(
		data: Result<Option<Vec<Vec<u8>>>>,
	) -> Result<Option<Vec<AvailExtrinsic>>> {
		let xts = data.map(|e| {
			e.map(|e| {
				e.iter()
					.enumerate()
					.map(|(i, raw)| {
						<_>::decode(&mut &raw[..])
							.context(format!("Couldn't decode AvailExtrinsic num {i}"))
					})
					.collect::<Result<Vec<AvailExtrinsic>>>()
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
	let res = match decode_app_data_to_extrinsics(get_decoded_data_from_db(
		db,
		cfg.app_id.unwrap(),
		block_num,
	)) {
		Ok(Some(data)) => ClientResponse::Normal(ExtrinsicsDataResponse {
			block: block_num,
			extrinsics: data,
		}),

		Ok(None) => match counter {
			lock if block_num < lock => ClientResponse::NotFinalized,
			lock if block_num == lock => ClientResponse::InProcess,
			_ => ClientResponse::NotFound,
		},
		Err(e) => ClientResponse::Error(e),
	};
	info!("Returning AppData: {res:?}");
	res
}

impl<T: Send + Serialize> warp::Reply for ClientResponse<T> {
	fn into_response(self) -> warp::reply::Response {
		match self {
			ClientResponse::Normal(response) => {
				warp::reply::with_status(warp::reply::json(&response), StatusCode::OK)
					.into_response()
			},
			ClientResponse::NotFound => {
				warp::reply::with_status(warp::reply::json(&"Not found"), StatusCode::NOT_FOUND)
					.into_response()
			},
			ClientResponse::NotFinalized => warp::reply::with_status(
				warp::reply::json(&"Not synced".to_owned()),
				StatusCode::BAD_REQUEST,
			)
			.into_response(),
			ClientResponse::InProcess => warp::reply::with_status(
				warp::reply::json(&"Not synced".to_owned()),
				StatusCode::UNAUTHORIZED,
			)
			.into_response(),
			ClientResponse::Error(e) => warp::reply::with_status(
				warp::reply::json(&e.to_string()),
				StatusCode::INTERNAL_SERVER_ERROR,
			)
			.into_response(),
		}
	}
}

pub async fn run_server(
	store: Arc<DB>,
	cfg: RuntimeConfig,
	_cell_query_tx: SyncSender<CellContentQueryPayload>,
	counter: Arc<Mutex<u64>>,
) {
	let host = cfg.http_server_host.clone();
	let port = cfg.http_server_port;

	let get_mode =
		warp::path!("v1" / "mode").map(move || warp::reply::json(&Mode::from(cfg.app_id)));
	
	let counter_clone = counter.clone();
	let get_latest_block =
		warp::path!("v1" / "latest_block").map(move || latest_block(counter_clone.clone()));

	let counter_confidence = counter.clone();
	let db = store.clone();
	let get_confidence = warp::path!("v1" / "confidence" / u64).map(move |block_num| {
		let counter_lock = counter_confidence.lock().unwrap();
		confidence(block_num, db.clone(), *counter_lock)
	});

	let db = store.clone();
	let cfg1 = cfg.clone();
	let counter_appdata = counter.clone();
	let get_appdata = warp::path!("v1" / "appdata" / u64).map(move |block_num| {
		let counter_lock = counter_appdata.lock().unwrap();
		appdata(block_num, db.clone(), cfg1.clone(), *counter_lock)
	});

	let cfg = cfg.clone();
	let db = store.clone();
	let counter_status = counter.clone();
	let get_status = warp::path!("v1" / "status" ).map(move || {
		let counter_lock = counter_status.lock().unwrap();
		status(&cfg, *counter_lock, db.clone())
	});

	let routes = warp::get().and(
		get_mode
			.or(get_latest_block)
			.or(get_confidence)
			.or(get_appdata)
			.or(get_status),
	);
	let addr = SocketAddr::from_str(format!("{host}:{port}").as_str())
		.context("Unable to parse host address from config")
		.unwrap();
	info!("RPC running on http://{host}:{port}");
	warp::serve(routes).run(addr).await;
}
