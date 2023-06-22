use crate::rpc;
use warp::{Filter, Rejection, Reply};

mod handlers;
mod types;

pub fn routes(
	version: String,
	network_version: String,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	let version = warp::path!("v2" / "version")
		.and(warp::any().map(move || version.clone()))
		.and(warp::any().map(move || network_version.clone()))
		.map(handlers::version);
	warp::get().and(version)
}
