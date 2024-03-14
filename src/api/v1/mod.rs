use crate::{data::Database, types::State};

use self::types::AppDataQuery;
use std::{
	convert::Infallible,
	sync::{Arc, Mutex},
};
use warp::{Filter, Rejection, Reply};

mod handlers;
mod types;

fn with_state(
	state: Arc<Mutex<State>>,
) -> impl Filter<Extract = (Arc<Mutex<State>>,), Error = Infallible> + Clone {
	warp::any().map(move || state.clone())
}

fn with_db<T: Database + Clone + Send>(
	db: T,
) -> impl Filter<Extract = (T,), Error = Infallible> + Clone {
	warp::any().map(move || db.clone())
}

fn with_app_id(
	app_id: Option<u32>,
) -> impl Filter<Extract = (Option<u32>,), Error = Infallible> + Clone {
	warp::any().map(move || app_id)
}

pub fn routes(
	db: impl Database + Clone + Send,
	app_id: Option<u32>,
	state: Arc<Mutex<State>>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	let mode = warp::path!("v1" / "mode")
		.and(with_app_id(app_id))
		.map(handlers::mode);

	let latest_block = warp::path!("v1" / "latest_block")
		.and(with_state(state.clone()))
		.map(handlers::latest_block);

	let confidence = warp::path!("v1" / "confidence" / u32)
		.and(with_db(db.clone()))
		.and(with_state(state.clone()))
		.map(handlers::confidence);

	let appdata = (warp::path!("v1" / "appdata" / u32))
		.and(warp::query::<AppDataQuery>())
		.and(with_db(db.clone()))
		.and(with_app_id(app_id))
		.and(with_state(state.clone()))
		.map(handlers::appdata);

	let status = warp::path!("v1" / "status")
		.and(with_app_id(app_id))
		.and(with_state(state))
		.and(with_db(db))
		.map(handlers::status);

	warp::get().and(mode.or(latest_block).or(confidence).or(appdata).or(status))
}
