use self::types::AppDataQuery;
use rocksdb::DB;
use std::{
	convert::Infallible,
	sync::{Arc, Mutex},
};
use warp::{Filter, Rejection, Reply};

mod handlers;
mod types;

fn with_counter(
	counter: Arc<Mutex<u32>>,
) -> impl Filter<Extract = (Arc<Mutex<u32>>,), Error = Infallible> + Clone {
	warp::any().map(move || counter.clone())
}

fn with_db(db: Arc<DB>) -> impl Filter<Extract = (Arc<DB>,), Error = Infallible> + Clone {
	warp::any().map(move || db.clone())
}

fn with_app_id(
	app_id: Option<u32>,
) -> impl Filter<Extract = (Option<u32>,), Error = Infallible> + Clone {
	warp::any().map(move || app_id)
}

pub fn routes(
	db: Arc<DB>,
	app_id: Option<u32>,
	counter: Arc<Mutex<u32>>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	let mode = warp::path!("v1" / "mode")
		.and(with_app_id(app_id))
		.map(handlers::mode);

	let latest_block = warp::path!("v1" / "latest_block")
		.and(with_counter(counter.clone()))
		.map(handlers::latest_block);

	let confidence = warp::path!("v1" / "confidence" / u32)
		.and(with_db(db.clone()))
		.and(with_counter(counter.clone()))
		.map(handlers::confidence);

	let appdata = (warp::path!("v1" / "appdata" / u32))
		.and(warp::query::<AppDataQuery>())
		.and(with_db(db.clone()))
		.and(with_app_id(app_id))
		.and(with_counter(counter.clone()))
		.map(handlers::appdata);

	let status = warp::path!("v1" / "status")
		.and(with_app_id(app_id))
		.and(with_counter(counter))
		.and(with_db(db))
		.map(handlers::status);

	warp::get().and(mode.or(latest_block).or(confidence).or(appdata).or(status))
}
