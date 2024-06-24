use std::{convert::Infallible, fmt::Display, sync::Arc};
use tokio::sync::broadcast;
use tracing::{debug, error, info};
use warp::{Filter, Rejection, Reply};

use self::{
	handlers::{handle_rejection, log_internal_server_error},
	types::{DataQuery, PublishMessage, Version, WsClients},
};

use crate::{
	api::v2::types::Topic,
	data::Database,
	network::{p2p, rpc::Client},
	types::{IdentityConfig, RuntimeConfig},
};

mod handlers;
mod transactions;
pub mod types;
mod ws;

async fn optionally<T>(value: Option<T>) -> Result<T, Rejection> {
	match value {
		Some(value) => Ok(value),
		None => Err(warp::reject::not_found()),
	}
}

fn with_db<T: Database + Clone + Send>(
	db: T,
) -> impl Filter<Extract = (T,), Error = Infallible> + Clone {
	warp::any().map(move || db.clone())
}

fn with_ws_clients(
	clients: WsClients,
) -> impl Filter<Extract = (WsClients,), Error = Infallible> + Clone {
	warp::any().map(move || clients.clone())
}

fn version_route(
	version: Version,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "version")
		.and(warp::get())
		.map(move || version.clone())
}

fn status_route(
	config: RuntimeConfig,
	db: impl Database + Clone + Send,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "status")
		.and(warp::get())
		.and(warp::any().map(move || config.clone()))
		.and(with_db(db))
		.map(handlers::status)
}

fn block_route(
	config: RuntimeConfig,
	db: impl Database + Clone + Send,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "blocks" / u32)
		.and(warp::get())
		.and(warp::any().map(move || config.clone()))
		.and(with_db(db))
		.then(handlers::block)
		.map(log_internal_server_error)
}

fn block_header_route(
	config: RuntimeConfig,
	db: impl Database + Clone + Send,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "blocks" / u32 / "header")
		.and(warp::get())
		.and(warp::any().map(move || config.clone()))
		.and(with_db(db))
		.then(handlers::block_header)
		.map(log_internal_server_error)
}

fn block_data_route(
	config: RuntimeConfig,
	db: impl Database + Clone + Send,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "blocks" / u32 / "data")
		.and(warp::get())
		.and(warp::query::<DataQuery>())
		.and(warp::any().map(move || config.clone()))
		.and(with_db(db))
		.then(handlers::block_data)
		.map(log_internal_server_error)
}

fn submit_route(
	submitter: Option<Arc<impl transactions::Submit + Clone + Send + Sync>>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "submit")
		.and(warp::post())
		.and_then(move || optionally(submitter.clone()))
		.and(warp::body::json())
		.then(handlers::submit)
		.map(log_internal_server_error)
}

fn p2p_local_info_route(
	p2p_client: p2p::Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "p2p" / "local" / "info")
		.and(warp::get())
		.and(warp::any().map(move || p2p_client.clone()))
		.then(handlers::p2p::get_peer_info)
		.map(log_internal_server_error)
}

fn p2p_peer_multiaddr_route(
	p2p_client: p2p::Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "p2p" / "peers" / "get-multiaddress")
		.and(warp::post())
		.and(warp::any().map(move || p2p_client.clone()))
		.and(warp::body::json())
		.then(handlers::p2p::get_peer_multiaddr)
		.map(log_internal_server_error)
}

fn p2p_peers_dial_route(
	p2p_client: p2p::Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "p2p" / "peers" / "dial")
		.and(warp::post())
		.and(warp::any().map(move || p2p_client.clone()))
		.and(warp::body::json())
		.then(handlers::p2p::dial_external_peer)
		.map(log_internal_server_error)
}

fn subscriptions_route(
	clients: WsClients,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "subscriptions")
		.and(warp::post())
		.and(warp::body::json())
		.and(with_ws_clients(clients))
		.and_then(handlers::subscriptions)
}

fn ws_route(
	clients: WsClients,
	version: Version,
	config: RuntimeConfig,
	submitter: Option<Arc<impl transactions::Submit + Clone + Send + Sync + 'static>>,
	db: impl Database + Clone + Send + 'static,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "ws" / String)
		.and(warp::ws())
		.and(with_ws_clients(clients))
		.and(warp::any().map(move || version.clone()))
		.and(warp::any().map(move || config.clone()))
		.and(warp::any().map(move || submitter.clone()))
		.and(with_db(db))
		.and_then(handlers::ws)
}

pub async fn publish<T: Clone + TryInto<PublishMessage>>(
	topic: Topic,
	mut receiver: broadcast::Receiver<T>,
	clients: WsClients,
) where
	<T as TryInto<PublishMessage>>::Error: Display,
{
	loop {
		let message = match receiver.recv().await {
			Ok(value) => value,
			Err(error) => {
				error!(?topic, "Cannot receive message: {error}");
				return;
			},
		};

		let message: PublishMessage = match message.try_into() {
			Ok(message) => message,
			Err(error) => {
				error!(?topic, "Cannot create message: {error}");
				continue;
			},
		};

		match clients.publish(&topic, message).await {
			Ok(results) => {
				let published = results.iter().filter(|&result| result.is_ok()).count();
				let failed = results.iter().filter(|&result| result.is_err()).count();
				info!(?topic, published, failed, "Message published to clients");
				for error in results.into_iter().filter_map(Result::err) {
					debug!(?topic, "Cannot publish message to client: {error}")
				}
			},
			Err(error) => error!(?topic, "Cannot publish message: {error}"),
		}
	}
}

#[allow(clippy::too_many_arguments)]
pub fn routes(
	version: String,
	network_version: String,
	config: RuntimeConfig,
	identity_config: IdentityConfig,
	rpc_client: Client<impl Database + Send + Sync + Clone + 'static>,
	ws_clients: WsClients,
	db: impl Database + Clone + Send + 'static,
	p2p_client: p2p::Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	let version = Version {
		version,
		network_version,
	};

	let app_id = config.app_id.as_ref();

	let submitter = app_id.map(|&app_id| {
		Arc::new(transactions::Submitter {
			rpc_client,
			app_id,
			signer: identity_config.avail_key_pair,
		})
	});

	version_route(version.clone())
		.or(status_route(config.clone(), db.clone()))
		.or(block_route(config.clone(), db.clone()))
		.or(block_header_route(config.clone(), db.clone()))
		.or(block_data_route(config.clone(), db.clone()))
		.or(subscriptions_route(ws_clients.clone()))
		.or(submit_route(submitter.clone()))
		.or(ws_route(ws_clients, version, config, submitter, db.clone()))
		.or(p2p_local_info_route(p2p_client.clone()))
		.or(p2p_peers_dial_route(p2p_client.clone()))
		.or(p2p_peer_multiaddr_route(p2p_client.clone()))
		.recover(handle_rejection)
}

#[cfg(test)]
mod tests {
	use super::{transactions, types::Transaction};
	use crate::{
		api::v2::types::{
			DataField, ErrorCode, SubmitResponse, Subscription, SubscriptionId, Topic, Version,
			WsClients, WsError, WsResponse,
		},
		data::{
			self, AchievedConfidenceKey, AchievedSyncConfidenceKey, AppDataKey, BlockHeaderKey,
			Database, IsSyncedKey, LatestHeaderKey, LatestSyncKey, MemoryDB, VerifiedCellCountKey,
			VerifiedDataKey, VerifiedHeaderKey, VerifiedSyncDataKey,
		},
		types::{BlockRange, RuntimeConfig},
	};
	use async_trait::async_trait;
	use avail_subxt::{api::runtime_types::avail_core::AppId, utils::H256};
	use avail_subxt::{
		api::runtime_types::avail_core::{
			data_lookup::compact::{CompactDataLookup, DataLookupItem},
			header::extension::{v3, HeaderExtension},
			kate_commitment::v3::KateCommitment,
		},
		primitives::Header as DaHeader,
	};
	use hyper::StatusCode;
	use kate_recovery::matrix::Partition;
	use std::{collections::HashSet, str::FromStr, sync::Arc};
	use subxt::config::substrate::Digest;
	use test_case::test_case;
	use uuid::Uuid;

	fn v1() -> Version {
		Version {
			version: "v1.0.0".to_string(),
			network_version: "nv1.0.0".to_string(),
		}
	}

	const NETWORK: &str = "{host}/{system_version}/0";

	#[tokio::test]
	async fn version_route() {
		let route = super::version_route(v1());
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

	#[tokio::test]
	async fn status_route_defaults() {
		let db = MemoryDB::default();
		let route = super::status_route(RuntimeConfig::default(), db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/status")
			.reply(&route)
			.await;

		let gen_hash = H256::default();
		let expected = format!(
			r#"{{"modes":["light"],"genesis_hash":"{:x?}","network":"{NETWORK}","blocks":{{"latest":0}}}}"#,
			gen_hash
		);
		assert_eq!(response.body(), &expected);
	}

	#[tokio::test]
	async fn status_route() {
		let runtime_config = RuntimeConfig {
			app_id: Some(1),
			sync_start_block: Some(10),
			block_matrix_partition: Some(Partition {
				number: 1,
				fraction: 10,
			}),
			..Default::default()
		};
		let db = MemoryDB::default();

		_ = db.put(IsSyncedKey, false);
		_ = db.put(LatestHeaderKey, 30);

		let mut achieved_confidence = BlockRange::init(20);
		achieved_confidence.last = 29;
		_ = db.put(AchievedConfidenceKey, achieved_confidence);

		let mut verified_sync_data = BlockRange::init(10);
		verified_sync_data.last = 18;
		_ = db.put(VerifiedSyncDataKey, verified_sync_data);

		let mut verified_data = BlockRange::init(20);
		verified_data.last = 29;
		_ = db.put(VerifiedDataKey, verified_data.clone());

		let mut achieved_sync_confidence = BlockRange::init(10);
		achieved_sync_confidence.last = 19;
		_ = db.put(AchievedSyncConfidenceKey, achieved_sync_confidence);

		let route = super::status_route(runtime_config, db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/status")
			.reply(&route)
			.await;

		let gen_hash = H256::default();
		let expected = format!(
			r#"{{"modes":["light","app","partition"],"app_id":1,"genesis_hash":"{:#x}","network":"{NETWORK}","blocks":{{"latest":30,"available":{{"first":20,"last":29}},"app_data":{{"first":20,"last":29}},"historical_sync":{{"synced":false,"available":{{"first":10,"last":19}},"app_data":{{"first":10,"last":18}}}}}},"partition":"1/10"}}"#,
			gen_hash
		);
		assert_eq!(response.body(), &expected);
	}

	#[test_case(1, 2)]
	#[test_case(10, 11)]
	#[test_case(10, 20)]
	#[tokio::test]
	async fn block_route_not_found(latest: u32, block_number: u32) {
		let config = RuntimeConfig::default();
		let db = data::MemoryDB::default();
		_ = db.put(LatestHeaderKey, latest);
		let route = super::block_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path(&format!("/v2/blocks/{block_number}"))
			.reply(&route)
			.await;

		assert_eq!(response.status(), StatusCode::NOT_FOUND);
	}

	#[tokio::test]
	async fn block_route_incomplete() {
		let config = RuntimeConfig::default();
		let db = data::MemoryDB::default();
		_ = db.put(LatestHeaderKey, 10);
		_ = db.put(VerifiedHeaderKey, BlockRange::init(10));
		_ = db.put(VerifiedDataKey, BlockRange::init(10));
		_ = db.put(BlockHeaderKey(10), incomplete_header());
		let route = super::block_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/blocks/10")
			.reply(&route)
			.await;

		assert_eq!(response.status(), StatusCode::OK);
		assert_eq!(
			response.body(),
			r#"{"status":"incomplete","confidence":null}"#
		);
	}

	#[tokio::test]
	async fn block_route_finished() {
		let config = RuntimeConfig::default();
		let db = data::MemoryDB::default();
		_ = db.put(LatestHeaderKey, 10);
		_ = db.put(VerifiedHeaderKey, BlockRange::init(10));
		_ = db.put(VerifiedDataKey, BlockRange::init(10));
		_ = db.put(VerifiedCellCountKey(10), 4);
		_ = db.put(BlockHeaderKey(10), header());
		let route = super::block_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/blocks/10")
			.reply(&route)
			.await;

		assert_eq!(response.status(), StatusCode::OK);
		assert_eq!(
			response.body(),
			r#"{"status":"finished","confidence":93.75}"#
		);
	}

	#[test_case(0, r#"Block header is not available"#  ; "Block is unavailable")]
	#[test_case(6, r#"Block header is not available"#  ; "Block is pending")]
	#[test_case(10, r#"Block header is not available"#  ; "Block is in verifying-header state")]
	#[tokio::test]
	async fn block_header_route_bad_request(block_number: u32, expected: &str) {
		let config = RuntimeConfig {
			sync_start_block: Some(1),
			..Default::default()
		};
		let db = data::MemoryDB::default();
		_ = db.put(LatestHeaderKey, 10);
		_ = db.put(VerifiedHeaderKey, BlockRange::init(9));
		_ = db.put(LatestSyncKey, 5);
		_ = db.put(BlockHeaderKey(block_number), header());
		let route = super::block_header_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path(&format!("/v2/blocks/{block_number}/header"))
			.reply(&route)
			.await;
		assert_eq!(response.status(), StatusCode::BAD_REQUEST);
		assert_eq!(response.body(), expected);
	}

	#[tokio::test]
	async fn block_header_route_not_found() {
		let config = RuntimeConfig::default();
		let db = data::MemoryDB::default();
		_ = db.put(LatestHeaderKey, 10);
		let route = super::block_header_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/blocks/11/header")
			.reply(&route)
			.await;
		assert_eq!(response.status(), StatusCode::NOT_FOUND);
	}

	fn header() -> DaHeader {
		DaHeader {
			parent_hash: H256::default(),
			number: 1,
			state_root: H256::default(),
			extrinsics_root: H256::default(),
			extension: HeaderExtension::V3(v3::HeaderExtension {
				commitment: KateCommitment::default(),
				app_lookup: CompactDataLookup {
					size: 1,
					index: vec![],
				},
			}),
			digest: Digest { logs: vec![] },
		}
	}

	fn incomplete_header() -> DaHeader {
		DaHeader {
			parent_hash: H256::default(),
			number: 1,
			state_root: H256::default(),
			extrinsics_root: H256::default(),
			extension: HeaderExtension::V3(v3::HeaderExtension {
				commitment: KateCommitment::default(),
				app_lookup: CompactDataLookup {
					size: 0,
					index: vec![DataLookupItem {
						app_id: AppId(0),
						start: 0,
					}],
				},
			}),
			digest: Digest { logs: vec![] },
		}
	}

	#[tokio::test]
	async fn block_header_route_ok() {
		let config = RuntimeConfig::default();
		let db = data::MemoryDB::default();
		_ = db.put(LatestHeaderKey, 1);
		_ = db.put(VerifiedHeaderKey, BlockRange::init(1));
		_ = db.put(BlockHeaderKey(1), header());
		let route = super::block_header_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/blocks/1/header")
			.reply(&route)
			.await;
		assert_eq!(
			response.body(),
			r#"{"hash":"0xadf25a1a5d969bb9c9bb9b2e95fe74b0093f0a49ac61e96a1cf41783127f9d1b","parent_hash":"0x0000000000000000000000000000000000000000000000000000000000000000","number":1,"state_root":"0x0000000000000000000000000000000000000000000000000000000000000000","extrinsics_root":"0x0000000000000000000000000000000000000000000000000000000000000000","extension":{"rows":0,"cols":0,"data_root":"0x0000000000000000000000000000000000000000000000000000000000000000","commitments":[],"app_lookup":{"size":1,"index":[]}}}"#
		);
	}

	#[test_case(0, r#"Block data is not available"#  ; "Block is unavailable")]
	#[test_case(6, r#"Block data is not available"#  ; "Block is pending")]
	#[test_case(8, r#"Block data is not available"#  ; "Block is in verifying-data state")]
	#[test_case(9, r#"Block data is not available"#  ; "Block is in verifying-confidence state")]
	#[test_case(10, r#"Block data is not available"#  ; "Block is in verifying-header state")]
	#[tokio::test]
	async fn block_data_route_bad_request(block_number: u32, expected: &str) {
		let config = RuntimeConfig {
			app_id: Some(1),
			sync_start_block: Some(1),
			..Default::default()
		};
		let db = data::MemoryDB::default();
		_ = db.put(LatestHeaderKey, 10);
		_ = db.put(VerifiedHeaderKey, BlockRange::init(10));
		_ = db.put(AchievedConfidenceKey, BlockRange::init(9));
		_ = db.put(VerifiedDataKey, BlockRange::init(8));
		_ = db.put(LatestSyncKey, 5);
		_ = db.put(BlockHeaderKey(block_number), header());
		let route = super::block_data_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path(&format!("/v2/blocks/{block_number}/data"))
			.reply(&route)
			.await;
		assert_eq!(response.status(), StatusCode::BAD_REQUEST);
		assert_eq!(response.body(), expected);
	}

	#[tokio::test]
	async fn block_data_route_not_found() {
		let config = RuntimeConfig::default();
		let db = data::MemoryDB::default();
		_ = db.put(LatestHeaderKey, 10);
		let route = super::block_data_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/blocks/11/data")
			.reply(&route)
			.await;
		assert_eq!(response.status(), StatusCode::NOT_FOUND);
	}

	#[tokio::test]
	async fn block_data_route_ok_empty() {
		let config = RuntimeConfig {
			app_id: Some(1),
			..Default::default()
		};
		let db = data::MemoryDB::default();
		_ = db.put(LatestHeaderKey, 10);
		_ = db.put(VerifiedHeaderKey, BlockRange::init(5));
		_ = db.put(AchievedConfidenceKey, BlockRange::init(5));
		_ = db.put(VerifiedDataKey, BlockRange::init(5));
		_ = db.put(BlockHeaderKey(5), header());
		let route = super::block_data_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/blocks/5/data")
			.reply(&route)
			.await;
		assert_eq!(response.status(), StatusCode::OK);
		assert_eq!(
			response.body(),
			r#"{"block_number":5,"data_transactions":[]}"#
		);
	}

	#[tokio::test]
	async fn block_data_route_ok() {
		let config = RuntimeConfig {
			app_id: Some(1),
			..Default::default()
		};
		let db = data::MemoryDB::default();
		_ = db.put(LatestHeaderKey, 10);
		_ = db.put(VerifiedHeaderKey, BlockRange::init(5));
		_ = db.put(AchievedConfidenceKey, BlockRange::init(5));
		_ = db.put(VerifiedDataKey, BlockRange::init(5));
		_ = db.put(
			AppDataKey(1, 5),
			vec![vec![
				189, 1, 132, 0, 212, 53, 147, 199, 21, 253, 211, 28, 97, 20, 26, 189, 4, 169, 159,
				214, 130, 44, 133, 88, 133, 76, 205, 227, 154, 86, 132, 231, 165, 109, 162, 125, 1,
				50, 12, 43, 176, 19, 42, 23, 73, 70, 223, 198, 180, 103, 34, 60, 246, 184, 49, 140,
				113, 174, 234, 229, 95, 71, 18, 92, 158, 185, 168, 140, 126, 12, 191, 156, 50, 234,
				8, 4, 68, 137, 5, 156, 94, 209, 7, 169, 105, 62, 63, 1, 122, 253, 195, 112, 173,
				239, 21, 73, 163, 240, 106, 109, 131, 0, 4, 0, 4, 29, 1, 20, 116, 101, 115, 116,
				10,
			]],
		);
		_ = db.put(BlockHeaderKey(5), header());
		let route = super::block_data_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/blocks/5/data")
			.reply(&route)
			.await;
		assert_eq!(response.status(), StatusCode::OK);
		assert_eq!(
			response.body(),
			r#"{"block_number":5,"data_transactions":[{"data":"dGVzdAo=","extrinsic":"vQGEANQ1k8cV/dMcYRQavQSpn9aCLIVYhUzN45pWhOelbaJ9ATIMK7ATKhdJRt/GtGciPPa4MYxxrurlX0cSXJ65qIx+DL+cMuoIBESJBZxe0QepaT4/AXr9w3Ct7xVJo/BqbYMABAAEHQEUdGVzdAo="}]}"#
		);
	}

	fn all_topics() -> HashSet<Topic> {
		vec![
			Topic::HeaderVerified,
			Topic::ConfidenceAchieved,
			Topic::DataVerified,
		]
		.into_iter()
		.collect()
	}

	fn all_data_fields() -> HashSet<DataField> {
		vec![DataField::Extrinsic, DataField::Data]
			.into_iter()
			.collect()
	}

	#[derive(Clone)]
	struct MockSubmitter {}

	#[async_trait]
	impl transactions::Submit for MockSubmitter {
		async fn submit(&self, _: Transaction) -> color_eyre::Result<SubmitResponse> {
			Ok(SubmitResponse {
				block_number: 0,
				block_hash: H256::random(),
				hash: H256::random(),
				index: 0,
			})
		}
	}

	#[test_case(r#"{"raw":""}"#, b"Request body deserialize error: unknown variant `raw`" ; "Invalid json schema")]
	#[test_case(r#"{"data":"dHJhbnooNhY3Rpb24:"}"#, b"Request body deserialize error: Invalid byte" ; "Invalid base64 value")]
	#[tokio::test]
	async fn submit_route_bad_request(json: &str, message: &[u8]) {
		let route = super::submit_route(Some(Arc::new(MockSubmitter {})));
		let response = warp::test::request()
			.method("POST")
			.path("/v2/submit")
			.body(json)
			.reply(&route)
			.await;
		assert_eq!(response.status(), StatusCode::BAD_REQUEST);
		assert!(response.body().starts_with(message));
	}

	#[test_case(r#"{"data":"dHJhbnNhY3Rpb24K"}"# ; "No errors in case of submitted data")]
	#[test_case(r#"{"extrinsic":"dHJhbnNhY3Rpb24K"}"# ; "No errors in case of submitted extrinsic")]
	#[tokio::test]
	async fn submit_route_extrinsic(body: &str) {
		let route = super::submit_route(Some(Arc::new(MockSubmitter {})));
		let response = warp::test::request()
			.method("POST")
			.path("/v2/submit")
			.body(body)
			.reply(&route)
			.await;
		assert_eq!(response.status(), StatusCode::OK);
		let response: SubmitResponse = serde_json::from_slice(response.body()).unwrap();
		let _ = serde_json::to_string(&response).unwrap();
	}

	#[tokio::test]
	async fn subscriptions_route() {
		let clients = WsClients::default();
		let route = super::subscriptions_route(clients.clone());

		let body = r#"{"topics":["confidence-achieved","data-verified","header-verified"],"data_fields":["data","extrinsic"]}"#;
		let response = warp::test::request()
			.method("POST")
			.body(body)
			.path("/v2/subscriptions")
			.reply(&route)
			.await;

		let SubscriptionId { subscription_id } = serde_json::from_slice(response.body()).unwrap();
		assert!(uuid::Uuid::from_str(&subscription_id).is_ok());

		let clients = clients.0.read().await;
		let client = clients.get(&subscription_id).unwrap();

		let expected = Subscription {
			topics: all_topics(),
			data_fields: all_data_fields(),
		};
		assert!(client.subscription == expected);
	}

	struct MockSetup {
		ws_client: warp::test::WsClient,
		db: MemoryDB,
	}

	impl MockSetup {
		async fn new(config: RuntimeConfig, submitter: Option<MockSubmitter>) -> Self {
			let client_uuid = uuid::Uuid::new_v4().to_string();
			let clients = WsClients::default();
			clients
				.subscribe(&client_uuid, Subscription::default())
				.await;

			let db = MemoryDB::default();
			let route = super::ws_route(
				clients.clone(),
				v1(),
				config.clone(),
				submitter.map(Arc::new),
				db.clone(),
			);
			let ws_client = warp::test::ws()
				.path(&format!("/v2/ws/{client_uuid}"))
				.handshake(route)
				.await
				.expect("handshake");

			MockSetup { ws_client, db }
		}

		async fn ws_send_text(&mut self, message: &str) -> String {
			self.ws_client.send_text(message).await;
			let message = self.ws_client.recv().await.unwrap();
			message.to_str().unwrap().to_string()
		}
	}

	#[tokio::test]
	async fn ws_route_version() {
		let mut test = MockSetup::new(RuntimeConfig::default(), None).await;
		let request = r#"{"type":"version","request_id":"cae63fff-c4b8-4af9-b4fe-0605a5329aa0"}"#;
		let response = test.ws_send_text(request).await;
		assert_eq!(
			r#"{"topic":"version","request_id":"cae63fff-c4b8-4af9-b4fe-0605a5329aa0","message":{"version":"v1.0.0","network_version":"nv1.0.0"}}"#,
			response
		);
	}

	#[tokio::test]
	async fn ws_route_status() {
		let config = RuntimeConfig {
			app_id: Some(1),
			sync_start_block: Some(10),
			block_matrix_partition: Some(Partition {
				number: 1,
				fraction: 10,
			}),
			..Default::default()
		};

		let mut test = MockSetup::new(config, None).await;

		_ = test.db.put(LatestHeaderKey, 30);
		_ = test.db.put(IsSyncedKey, false);

		let mut achieved_confidence = BlockRange::init(20);
		achieved_confidence.last = 29;
		_ = test.db.put(AchievedConfidenceKey, achieved_confidence);

		let mut verified_sync_data = BlockRange::init(10);
		verified_sync_data.last = 18;
		_ = test.db.put(VerifiedSyncDataKey, verified_sync_data);

		let mut verified_data = BlockRange::init(20);
		verified_data.last = 29;
		_ = test.db.put(VerifiedDataKey, verified_data);

		let mut achieved_sync_confidence = BlockRange::init(10);
		achieved_sync_confidence.last = 19;
		_ = test
			.db
			.put(AchievedSyncConfidenceKey, achieved_sync_confidence);

		let gen_hash = H256::default();
		let expected = format!(
			r#"{{"topic":"status","request_id":"363c71fc-90f7-4276-a5b6-bec688bf01e2","message":{{"modes":["light","app","partition"],"app_id":1,"genesis_hash":"{:x?}","network":"{NETWORK}","blocks":{{"latest":30,"available":{{"first":20,"last":29}},"app_data":{{"first":20,"last":29}},"historical_sync":{{"synced":false,"available":{{"first":10,"last":19}},"app_data":{{"first":10,"last":18}}}}}},"partition":"1/10"}}}}"#,
			gen_hash
		);

		let status_request =
			r#"{"type":"status","request_id":"363c71fc-90f7-4276-a5b6-bec688bf01e2"}"#;
		assert_eq!(expected, test.ws_send_text(status_request).await);
	}

	#[test_case("",  "Failed to parse request" ; "Empty request")]
	#[test_case("abcd",  "Failed to parse request" ; "Invalid json")]
	#[test_case("{}",  "Failed to parse request" ; "Empty json")]
	#[test_case(r#"{"type":"unknown","request_id":"11043443-7e4c-4485-a21c-304b457b6cc7","message":""}"#,  "Failed to parse request: Cannot parse json" ; "Wrong request type")]
	#[tokio::test]
	async fn ws_route_bad_request(request: &str, expected: &str) {
		let mut test = MockSetup::new(RuntimeConfig::default(), None).await;
		let response = test.ws_send_text(request).await;
		assert!(response.contains(expected));
	}

	fn to_uuid(uuid: &str) -> Uuid {
		Uuid::try_parse(uuid).unwrap()
	}

	#[test_case(r#"{"type":"submit","request_id":"16b24956-2e01-4ba8-bad5-456c561c87d7","message":{"data":""}}"#, false, Some("16b24956-2e01-4ba8-bad5-456c561c87d7"), "Submit is not configured" ; "No submitter")]
	#[test_case(r#"{"type":"submit","request_id":"36bc1f28-e093-422f-964b-1cb1b3882baf","message":{"extrinsic":""}}"#, true, Some("36bc1f28-e093-422f-964b-1cb1b3882baf"), "Transaction is empty" ; "Empty extrinsic")]
	#[test_case(r#"{"type":"submit","request_id":"cc60b2f3-d9ff-4c73-9632-d21d07f7b620","message":{"data":""}}"#, true, Some("cc60b2f3-d9ff-4c73-9632-d21d07f7b620"), "Transaction is empty" ; "Empty data")]
	#[test_case(r#"{"type":"submit","request_id":"9181df86-22f0-42a1-a965-60adb9fc6bdc","message":{"extrinsic":"bad"}}"#, true, None, "Failed to parse request" ; "Bad extrinsic")]
	#[test_case(r#"{"type":"submit","request_id":"78cd7b7b-ba70-48e9-a1da-96b370db4d8f","message":{"data":"bad"}}"#, true, None, "Failed to parse request" ; "Bad data")]
	#[tokio::test]
	async fn ws_route_submit_bad_requests(
		request: &str,
		submitter: bool,
		expected_request_id: Option<&str>,
		expected: &str,
	) {
		let submitter = submitter.then_some(MockSubmitter {});
		let expected_request_id = expected_request_id.map(to_uuid);
		let mut test = MockSetup::new(RuntimeConfig::default(), submitter).await;
		let response = test.ws_send_text(request).await;
		let WsError::Error(error) = serde_json::from_str(&response).unwrap();
		assert_eq!(error.error_code, ErrorCode::BadRequest);
		assert_eq!(error.request_id, expected_request_id);
		assert!(error.message.contains(expected));
	}

	#[tokio::test]
	async fn ws_route_submit_data() {
		let submitter = Some(MockSubmitter {});
		let mut test = MockSetup::new(RuntimeConfig::default(), submitter).await;

		let request = r#"{"type":"submit","request_id":"fca2ff0c-7a26-42a2-a6f0-d0aeeaba8a9a","message":{"data":"dHJhbnNhY3Rpb24K"}}"#;
		let response = test.ws_send_text(request).await;

		let WsResponse::DataTransactionSubmitted(response) =
			serde_json::from_str(&response).unwrap()
		else {
			panic!("Invalid response");
		};
		let expected_request_id = to_uuid("fca2ff0c-7a26-42a2-a6f0-d0aeeaba8a9a");
		assert_eq!(response.request_id, expected_request_id);
		assert_eq!(response.message.index, 0);
	}

	#[tokio::test]
	async fn ws_route_submit_extrinsic() {
		let submitter = Some(MockSubmitter {});
		let mut test = MockSetup::new(RuntimeConfig::default(), submitter).await;

		let request = r#"{"type":"submit","request_id":"fca2ff0c-7a26-42a2-a6f0-d0aeeaba8a9a","message":{"extrinsic":"dHJhbnNhY3Rpb24K"}}"#;
		let response = test.ws_send_text(request).await;

		let WsResponse::DataTransactionSubmitted(response) =
			serde_json::from_str(&response).unwrap()
		else {
			panic!("Invalid response");
		};
		let expected_request_id = to_uuid("fca2ff0c-7a26-42a2-a6f0-d0aeeaba8a9a");
		assert_eq!(response.request_id, expected_request_id);
		assert_eq!(response.message.index, 0);
	}
}
