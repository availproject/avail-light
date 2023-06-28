use warp::{Filter, Rejection, Reply};

mod handlers;
mod types;

fn version_route(
	version: String,
	network_version: String,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "version")
		.and(warp::get())
		.and(warp::any().map(move || version.clone()))
		.and(warp::any().map(move || network_version.clone()))
		.map(handlers::version)
}

pub fn routes(
	version: String,
	network_version: String,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	version_route(version, network_version)
}

#[cfg(test)]
mod tests {
	#[tokio::test]
	async fn version_route() {
		let route = super::version_route("v1.0.0".to_string(), "nv1.0.0".to_string());
		let response = warp::test::request()
			.method("GET")
			.path("/v2/version")
			.reply(&route)
			.await;

		assert_eq!(
			response.body(),
			r#"{"version":"v1.0.0","network_version":"nv1.0.0"}"#
		);
	}
}
