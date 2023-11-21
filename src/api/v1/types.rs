use avail_subxt::primitives::AppUncheckedExtrinsic;
use color_eyre::Report;
use hyper::StatusCode;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum ClientResponse<T>
where
	T: Serialize,
{
	Normal(T),
	NotFound,
	NotFinalized,
	InProcess,
	Error(Report),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConfidenceResponse {
	pub block: u32,
	pub confidence: f64,
	pub serialised_confidence: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum Extrinsics {
	Encoded(Vec<AppUncheckedExtrinsic>),
	Decoded(Vec<String>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExtrinsicsDataResponse {
	pub block: u32,
	pub extrinsics: Extrinsics,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LatestBlockResponse {
	pub latest_block: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Status {
	pub block_num: u32,
	pub confidence: f64,
	pub app_id: Option<u32>,
}

#[derive(Deserialize, Serialize)]
pub struct AppDataQuery {
	pub decode: Option<bool>,
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
				warp::reply::json(&"Processing block".to_owned()),
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
