extern crate rocksdb;

use std::{
	convert::TryInto,
	net::SocketAddr,
	str::FromStr,
	sync::{mpsc::SyncSender, Arc},
};

use anyhow::{anyhow, Context, Result};
use codec::Decode;
use num::{BigUint, FromPrimitive};
use rocksdb::DB;
use warp::{http::StatusCode, Filter};

use crate::{
	app_client::AvailExtrinsic,
	types::{
		CellContentQueryPayload, ConfidenceResponse, ExtrinsicsDataResponse, Mode, RuntimeConfig,
	},
};

pub fn calculate_confidence(count: u32) -> f64 { 100f64 * (1f64 - 1f64 / 2u32.pow(count) as f64) }

pub fn serialised_confidence(block: u64, factor: f64) -> Option<String> {
	let block_big: BigUint = FromPrimitive::from_u64(block)?;
	let factor_big: BigUint = FromPrimitive::from_u64((10f64.powi(7) * factor) as u64)?;
	let shifted: BigUint = block_big << 32 | factor_big;
	Some(shifted.to_str_radix(10))
}

fn confidence(block_num: u64, db: Arc<DB>) -> Result<ConfidenceResponse> {
	fn get_confidence(db: Arc<DB>, block: u64) -> Result<u32> {
		let cf_handle = db
			.cf_handle(crate::consts::CONFIDENCE_FACTOR_CF)
			.context("Couldn't get column handle from db")?;

		db.get_cf(&cf_handle, block.to_be_bytes())
			.context("Couldn't get confidence in db")
			.and_then(|found| found.context("Couldn't find confidence in db"))
			.and_then(|confidence| {
				confidence
					.try_into()
					.map_err(|_| anyhow!("Conversion failed"))
					.context("Unable to convert confindence (wrong number of bytes)")
			})
			.map(u32::from_be_bytes)
	}

	let block = block_num;
	get_confidence(db.clone(), block)
		.context("Couldn't get confidence")
		.map(move |count| {
			let confidence = calculate_confidence(count);
			let serialised_confidence = serialised_confidence(block, confidence);
			ConfidenceResponse {
				block,
				confidence,
				serialised_confidence,
			}
		})
}

fn appdata(block_num: u64, db: Arc<DB>, cfg: &RuntimeConfig) -> Result<ExtrinsicsDataResponse> {
	fn get_data(db: Arc<DB>, app_id: u32, block: u64) -> Result<Vec<Vec<u8>>> {
		let key = format!("{app_id}:{block}");
		let cf_handle = db
			.cf_handle(crate::consts::APP_DATA_CF)
			.context("Couldn't get column handle from db")?;

		db.get_cf(&cf_handle, key.as_bytes())
			.context("Couldn't get app data in db")
			.and_then(|found| found.context("Couldn't find app data in db"))
			.and_then(|encoded_data| {
				<_>::decode(&mut &encoded_data[..]).context("Couldn't scale decode raw data)")
			})
	}

	let block = block_num;
	let app_id = cfg.app_id.unwrap();
	log::info!("fetching data for app_id: {app_id}");
	get_data(db.clone(), app_id, block)
		.context("Couldn't get app data from db")
		.and_then(|data| {
			let xts: Result<Vec<AvailExtrinsic>> = data
				.iter()
				.map(|raw| <_>::decode(&mut &raw[..]).context("Couldn't decode"))
				.collect();

			let resp = xts.map(|e| {
				let resp = ExtrinsicsDataResponse {
					block,
					extrinsics: e,
				};
				resp
			});

			resp.context("Couldn't decode extrinics")
		})
}

pub async fn run_server(
	store: Arc<DB>,
	cfg: RuntimeConfig,
	_cell_query_tx: SyncSender<CellContentQueryPayload>,
) {
	let host = cfg.http_server_host.clone();
	let port = cfg.http_server_port;

	let get_mode =
		warp::path!("v1" / "mode").map(move || warp::reply::json(&Mode::from(cfg.app_id)));

	let db = store.clone();
	let get_confidence = warp::path!("v1" / "confidence" / u64).map(move |block_num| {
		match confidence(block_num, db.clone()) {
			Ok(v) => warp::reply::with_status(warp::reply::json(&v), StatusCode::OK),
			Err(e) => warp::reply::with_status(
				warp::reply::json(&e.to_string()),
				StatusCode::INTERNAL_SERVER_ERROR,
			),
		}
	});

	let db = store.clone();
	let get_appdata = warp::path!("v1" / "appdata" / u64).map(move |block_num| {
		match appdata(block_num, db.clone(), &cfg) {
			Ok(v) => warp::reply::with_status(warp::reply::json(&v), StatusCode::OK),
			Err(e) => warp::reply::with_status(
				warp::reply::json(&e.to_string()),
				StatusCode::INTERNAL_SERVER_ERROR,
			),
		}
	});

	let routes = warp::get().and(get_mode.or(get_confidence).or(get_appdata));
	let addr = SocketAddr::from_str(format!("{host}:{port}").as_str())
		.context("Unable to parse host address from config")
		.unwrap();
	log::info!("RPC running on http://{host}:{port}");
	warp::serve(routes).run(addr).await;
}
