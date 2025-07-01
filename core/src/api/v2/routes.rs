use super::{handlers, transactions};
use crate::{
	api::{
		configuration::SharedConfig,
		server::log_internal_server_error,
		types::{DataQuery, WsClients},
	},
	data::Database,
};
use std::{convert::Infallible, sync::Arc};
use warp::{Filter, Rejection, Reply};

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

pub fn version_route(
	version: String,
	db: impl Database + Clone + Send,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "version")
		.and(warp::get())
		.map(move || version.clone())
		.and(with_db(db))
		.map(handlers::version)
}

pub fn status_route(
	config: SharedConfig,
	db: impl Database + Clone + Send,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "status")
		.and(warp::get())
		.and(warp::any().map(move || config.clone()))
		.and(with_db(db))
		.map(handlers::status)
}

pub fn block_route(
	config: SharedConfig,
	db: impl Database + Clone + Send,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "blocks" / u32)
		.and(warp::get())
		.and(warp::any().map(move || config.clone()))
		.and(with_db(db))
		.then(handlers::block)
		.map(log_internal_server_error)
}

pub fn block_header_route(
	config: SharedConfig,
	db: impl Database + Clone + Send,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "blocks" / u32 / "header")
		.and(warp::get())
		.and(warp::any().map(move || config.clone()))
		.and(with_db(db))
		.then(handlers::block_header)
		.map(log_internal_server_error)
}

pub fn block_data_route(
	config: SharedConfig,
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

pub fn submit_route(
	submitter: Option<Arc<impl transactions::Submit + Clone + Send + Sync>>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "submit")
		.and(warp::post())
		.and_then(move || optionally(submitter.clone()))
		.and(warp::body::json())
		.then(handlers::submit)
		.map(log_internal_server_error)
}

pub fn subscriptions_route(
	clients: WsClients,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "subscriptions")
		.and(warp::post())
		.and(warp::body::json())
		.and(with_ws_clients(clients))
		.and_then(handlers::subscriptions)
}

pub fn ws_route(
	clients: WsClients,
	version: String,
	config: SharedConfig,
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

#[cfg(test)]
mod tests {
	use super::transactions;
	use crate::{
		api::{
			configuration::SharedConfig,
			types::{
				DataField, ErrorCode, SubmitResponse, Subscription, SubscriptionId, Topic,
				Transaction, WsClients, WsError, WsResponse,
			},
		},
		data::{
			self, AchievedConfidenceKey, AchievedSyncConfidenceKey, AppDataKey, BlockHeaderKey,
			BlockHeaderReceivedAtKey, Database, IsSyncedKey, LatestHeaderKey, LatestSyncKey,
			MemoryDB, RpcNodeKey, VerifiedCellCountKey, VerifiedDataKey, VerifiedHeaderKey,
			VerifiedSyncDataKey,
		},
		network::rpc::Node,
		types::BlockRange,
	};
	use async_trait::async_trait;
	use avail_rust::{
		avail::runtime_types::avail_core::{
			data_lookup::compact::{CompactDataLookup, DataLookupItem},
			header::extension::{v3, HeaderExtension},
			kate_commitment::v3::KateCommitment,
			AppId,
		},
		subxt::config::substrate::Digest,
		AvailHeader, H256,
	};
	use hyper::StatusCode;
	use std::{collections::HashSet, str::FromStr, sync::Arc};

	use test_case::test_case;
	use uuid::Uuid;

	fn default_node() -> Node {
		Node {
			host: "host".to_string(),
			system_version: "nv1.0.0".to_string(),
			spec_version: 0,
			genesis_hash: H256::zero(),
		}
	}

	#[tokio::test]
	async fn version_route() {
		let db = MemoryDB::default();
		db.put(RpcNodeKey, default_node());
		let route = super::version_route("v1.0.0".to_string(), db);
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
		db.put(RpcNodeKey, default_node());
		let route = super::status_route(SharedConfig::default(), db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/status")
			.reply(&route)
			.await;

		let gen_hash = H256::default();
		let expected = format!(
			r#"{{"modes":["light"],"genesis_hash":"{gen_hash:x?}","network":"host/nv1.0.0/0","blocks":{{"latest":0}}}}"#
		);
		assert_eq!(response.body(), &expected);
	}

	#[tokio::test]
	async fn status_route() {
		let runtime_config = SharedConfig {
			app_id: Some(1),
			sync_start_block: Some(10),
			..Default::default()
		};
		let db = MemoryDB::default();
		db.put(RpcNodeKey, default_node());

		db.put(IsSyncedKey, false);
		db.put(LatestHeaderKey, 30);

		let mut achieved_confidence = BlockRange::init(20);
		achieved_confidence.last = 29;
		db.put(AchievedConfidenceKey, achieved_confidence);

		let mut verified_sync_data = BlockRange::init(10);
		verified_sync_data.last = 18;
		db.put(VerifiedSyncDataKey, verified_sync_data);

		let mut verified_data = BlockRange::init(20);
		verified_data.last = 29;
		db.put(VerifiedDataKey, verified_data.clone());

		let mut achieved_sync_confidence = BlockRange::init(10);
		achieved_sync_confidence.last = 19;
		db.put(AchievedSyncConfidenceKey, achieved_sync_confidence);

		let route = super::status_route(runtime_config, db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/status")
			.reply(&route)
			.await;

		let gen_hash = H256::default();
		let expected = format!(
			r#"{{"modes":["light","app"],"app_id":1,"genesis_hash":"{gen_hash:#x}","network":"host/nv1.0.0/0","blocks":{{"latest":30,"available":{{"first":20,"last":29}},"app_data":{{"first":20,"last":29}},"historical_sync":{{"synced":false,"available":{{"first":10,"last":19}},"app_data":{{"first":10,"last":18}}}}}}}}"#
		);
		assert_eq!(response.body(), &expected);
	}

	#[test_case(1, 2)]
	#[test_case(10, 11)]
	#[test_case(10, 20)]
	#[tokio::test]
	async fn block_route_not_found(latest: u32, block_number: u32) {
		let config = SharedConfig::default();
		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, latest);
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
		let config = SharedConfig::default();
		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 10);
		db.put(VerifiedHeaderKey, BlockRange::init(10));
		db.put(VerifiedDataKey, BlockRange::init(10));
		db.put(BlockHeaderKey(10), incomplete_header());
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
		let config = SharedConfig::default();
		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 10);
		db.put(VerifiedHeaderKey, BlockRange::init(10));
		db.put(VerifiedDataKey, BlockRange::init(10));
		db.put(VerifiedCellCountKey(10), 4);
		db.put(BlockHeaderKey(10), header());
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
		let config = SharedConfig {
			sync_start_block: Some(1),
			..Default::default()
		};
		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 10);
		db.put(VerifiedHeaderKey, BlockRange::init(9));
		db.put(LatestSyncKey, 5);
		db.put(BlockHeaderKey(block_number), header());
		db.put(BlockHeaderReceivedAtKey(block_number), 1737039274);
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
		let config = SharedConfig::default();
		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 10);
		let route = super::block_header_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/blocks/11/header")
			.reply(&route)
			.await;
		assert_eq!(response.status(), StatusCode::NOT_FOUND);
	}

	fn header() -> AvailHeader {
		AvailHeader {
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

	fn incomplete_header() -> AvailHeader {
		AvailHeader {
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
		let config = SharedConfig::default();
		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 1);
		db.put(VerifiedHeaderKey, BlockRange::init(1));
		db.put(BlockHeaderKey(1), header());
		db.put(BlockHeaderReceivedAtKey(1), 1737039274);
		let route = super::block_header_route(config, db);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/blocks/1/header")
			.reply(&route)
			.await;
		assert_eq!(
			response.body(),
			r#"{"hash":"0xadf25a1a5d969bb9c9bb9b2e95fe74b0093f0a49ac61e96a1cf41783127f9d1b","parent_hash":"0x0000000000000000000000000000000000000000000000000000000000000000","number":1,"state_root":"0x0000000000000000000000000000000000000000000000000000000000000000","extrinsics_root":"0x0000000000000000000000000000000000000000000000000000000000000000","extension":{"rows":0,"cols":0,"data_root":"0x0000000000000000000000000000000000000000000000000000000000000000","commitments":[],"app_lookup":{"size":1,"index":[]}},"digest":{"logs":[]},"received_at":1737039274}"#
		);
	}

	#[test_case(0, r#"Block data is not available"#  ; "Block is unavailable")]
	#[test_case(6, r#"Block data is not available"#  ; "Block is pending")]
	#[test_case(8, r#"Block data is not available"#  ; "Block is in verifying-data state")]
	#[test_case(9, r#"Block data is not available"#  ; "Block is in verifying-confidence state")]
	#[test_case(10, r#"Block data is not available"#  ; "Block is in verifying-header state")]
	#[tokio::test]
	async fn block_data_route_bad_request(block_number: u32, expected: &str) {
		let config = SharedConfig {
			app_id: Some(1),
			sync_start_block: Some(1),
			..Default::default()
		};
		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 10);
		db.put(VerifiedHeaderKey, BlockRange::init(10));
		db.put(AchievedConfidenceKey, BlockRange::init(9));
		db.put(VerifiedDataKey, BlockRange::init(8));
		db.put(LatestSyncKey, 5);
		db.put(BlockHeaderKey(block_number), header());
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
		let config = SharedConfig::default();
		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 10);
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
		let config = SharedConfig {
			app_id: Some(1),
			..Default::default()
		};
		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 10);
		db.put(VerifiedHeaderKey, BlockRange::init(5));
		db.put(AchievedConfidenceKey, BlockRange::init(5));
		db.put(VerifiedDataKey, BlockRange::init(5));
		db.put(BlockHeaderKey(5), header());
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
		let config = SharedConfig {
			app_id: Some(1),
			..Default::default()
		};
		let db = data::MemoryDB::default();
		db.put(LatestHeaderKey, 10);
		db.put(VerifiedHeaderKey, BlockRange::init(5));
		db.put(AchievedConfidenceKey, BlockRange::init(5));
		db.put(VerifiedDataKey, BlockRange::init(5));
		db.put(
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
		db.put(BlockHeaderKey(5), header());
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
		async fn new(config: SharedConfig, submitter: Option<MockSubmitter>) -> Self {
			let client_uuid = uuid::Uuid::new_v4().to_string();
			let clients = WsClients::default();
			clients
				.subscribe(&client_uuid, Subscription::default())
				.await;

			let db = MemoryDB::default();

			let node = Node {
				host: "host".to_string(),
				system_version: "nv1.0.0".to_string(),
				spec_version: 0,
				genesis_hash: H256::zero(),
			};

			db.put(RpcNodeKey, node);

			let route = super::ws_route(
				clients.clone(),
				"v1.0.0".to_string(),
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
		let mut test = MockSetup::new(SharedConfig::default(), None).await;
		let request = r#"{"type":"version","request_id":"cae63fff-c4b8-4af9-b4fe-0605a5329aa0"}"#;
		let response = test.ws_send_text(request).await;
		assert_eq!(
			r#"{"topic":"version","request_id":"cae63fff-c4b8-4af9-b4fe-0605a5329aa0","message":{"version":"v1.0.0","network_version":"nv1.0.0"}}"#,
			response
		);
	}

	#[tokio::test]
	async fn ws_route_status() {
		let config = SharedConfig {
			app_id: Some(1),
			sync_start_block: Some(10),
			..Default::default()
		};

		let mut test = MockSetup::new(config, None).await;

		test.db.put(LatestHeaderKey, 30);
		test.db.put(IsSyncedKey, false);

		let mut achieved_confidence = BlockRange::init(20);
		achieved_confidence.last = 29;
		test.db.put(AchievedConfidenceKey, achieved_confidence);

		let mut verified_sync_data = BlockRange::init(10);
		verified_sync_data.last = 18;
		test.db.put(VerifiedSyncDataKey, verified_sync_data);

		let mut verified_data = BlockRange::init(20);
		verified_data.last = 29;
		test.db.put(VerifiedDataKey, verified_data);

		let mut achieved_sync_confidence = BlockRange::init(10);
		achieved_sync_confidence.last = 19;
		test.db
			.put(AchievedSyncConfidenceKey, achieved_sync_confidence);

		let gen_hash = H256::default();
		let expected = format!(
			r#"{{"topic":"status","request_id":"363c71fc-90f7-4276-a5b6-bec688bf01e2","message":{{"modes":["light","app"],"app_id":1,"genesis_hash":"{gen_hash:x?}","network":"host/nv1.0.0/0","blocks":{{"latest":30,"available":{{"first":20,"last":29}},"app_data":{{"first":20,"last":29}},"historical_sync":{{"synced":false,"available":{{"first":10,"last":19}},"app_data":{{"first":10,"last":18}}}}}}}}}}"#
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
		let mut test = MockSetup::new(SharedConfig::default(), None).await;
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
		let mut test = MockSetup::new(SharedConfig::default(), submitter).await;
		let response = test.ws_send_text(request).await;
		let WsError::Error(error) = serde_json::from_str(&response).unwrap();
		assert_eq!(error.error_code, ErrorCode::BadRequest);
		assert_eq!(error.request_id, expected_request_id);
		assert!(error.message.contains(expected));
	}

	#[tokio::test]
	async fn ws_route_submit_data() {
		let submitter = Some(MockSubmitter {});
		let mut test = MockSetup::new(SharedConfig::default(), submitter).await;

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
		let mut test = MockSetup::new(SharedConfig::default(), submitter).await;

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
