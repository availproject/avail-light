use self::{
	handlers::handle_rejection,
	types::{Clients, Version},
};
use crate::{
	rpc::Node,
	types::{RuntimeConfig, State},
};
use avail_subxt::avail;
use std::{
	collections::HashMap,
	convert::Infallible,
	sync::{Arc, Mutex},
};
use tokio::sync::RwLock;
use warp::{Filter, Rejection, Reply};

mod handlers;
mod transactions;
mod types;
mod ws;

async fn optionally<T>(value: Option<T>) -> Result<T, Rejection> {
	match value {
		Some(value) => Ok(value),
		None => Err(warp::reject::not_found()),
	}
}

fn with_clients(clients: Clients) -> impl Filter<Extract = (Clients,), Error = Infallible> + Clone {
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
	node: Node,
	state: Arc<Mutex<State>>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "status")
		.and(warp::get())
		.and(warp::any().map(move || config.clone()))
		.and(warp::any().map(move || node.clone()))
		.and(warp::any().map(move || state.clone()))
		.map(handlers::status)
}

fn submit_route(
	submitter: Option<Arc<impl transactions::Submit + Clone + Send + Sync>>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "submit")
		.and(warp::post())
		.and_then(move || optionally(submitter.clone()))
		.and(warp::body::json())
		.then(handlers::submit)
		.map(types::handle_result)
}

fn subscriptions_route(
	clients: Clients,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "subscriptions")
		.and(warp::post())
		.and(warp::body::json())
		.and(with_clients(clients))
		.and_then(handlers::subscriptions)
}

fn ws_route(
	clients: Clients,
	version: Version,
	config: RuntimeConfig,
	node: Node,
	submitter: Option<Arc<impl transactions::Submit + Clone + Send + Sync + 'static>>,
	state: Arc<Mutex<State>>,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "ws" / String)
		.and(warp::ws())
		.and(with_clients(clients))
		.and(warp::any().map(move || version.clone()))
		.and(warp::any().map(move || config.clone()))
		.and(warp::any().map(move || node.clone()))
		.and(warp::any().map(move || submitter.clone()))
		.and(warp::any().map(move || state.clone()))
		.and_then(handlers::ws)
}

pub fn routes(
	version: String,
	network_version: String,
	node: Node,
	state: Arc<Mutex<State>>,
	config: RuntimeConfig,
	node_client: avail::Client,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	let clients: Clients = Arc::new(RwLock::new(HashMap::new()));
	let version = Version {
		version,
		network_version,
	};

	let app_id = config.app_id.as_ref();
	let pair_signer = config.avail_secret_key.clone().map(From::from);

	let submitter = app_id.map(|&app_id| {
		Arc::new(transactions::Submitter {
			node_client,
			app_id,
			pair_signer,
		})
	});

	version_route(version.clone())
		.or(status_route(config.clone(), node.clone(), state.clone()))
		.or(subscriptions_route(clients.clone()))
		.or(submit_route(submitter.clone()))
		.or(ws_route(clients, version, config, node, submitter, state))
		.recover(handle_rejection)
}

#[cfg(test)]
mod tests {
	use super::{
		submit_route, transactions,
		types::{Client, Transaction},
	};
	use crate::{
		api::v2::types::{
			Clients, DataFields, ErrorCode, SubmitResponse, Subscription, SubscriptionId, Topics,
			Version, WsError, WsResponse,
		},
		rpc::Node,
		types::{RuntimeConfig, State},
	};
	use async_trait::async_trait;
	use hyper::StatusCode;
	use kate_recovery::matrix::Partition;
	use sp_core::H256;
	use std::{
		collections::{HashMap, HashSet},
		str::FromStr,
		sync::{Arc, Mutex},
	};
	use test_case::test_case;
	use tokio::sync::RwLock;
	use uuid::Uuid;
	use warp::test::WsClient;

	fn v1() -> Version {
		Version {
			version: "v1.0.0".to_string(),
			network_version: "nv1.0.0".to_string(),
		}
	}

	const GENESIS_HASH: &str = "0xc590b3c924c35c2f241746522284e4709df490d73a38aaa7d6de4ed1eac2f500";
	const NETWORK: &str = "{host}/{system_version}/data-avail/0";

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

	impl Default for Node {
		fn default() -> Self {
			Self {
				host: "{host}".to_string(),
				system_version: "{system_version}".to_string(),
				spec_version: 0,
				genesis_hash: H256::from_str(GENESIS_HASH).unwrap(),
			}
		}
	}

	#[tokio::test]
	async fn status_route_defaults() {
		let state = Arc::new(Mutex::new(State::default()));
		let route = super::status_route(RuntimeConfig::default(), Node::default(), state);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/status")
			.reply(&route)
			.await;

		let expected = format!(
			r#"{{"modes":["light"],"genesis_hash":"{GENESIS_HASH}","network":"{NETWORK}","blocks":{{"latest":0}}}}"#
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
		let state = Arc::new(Mutex::new(State::default()));
		{
			let mut state = state.lock().unwrap();
			state.latest = 30;
			state.set_confidence_achieved(20);
			state.set_confidence_achieved(29);
			state.set_data_verified(20);
			state.set_data_verified(29);
			state.set_synced(false);
			state.set_sync_confidence_achieved(10);
			state.set_sync_confidence_achieved(19);
			state.set_sync_data_verified(10);
			state.set_sync_data_verified(18);
		}

		let route = super::status_route(runtime_config, Node::default(), state);
		let response = warp::test::request()
			.method("GET")
			.path("/v2/status")
			.reply(&route)
			.await;

		let expected = format!(
			r#"{{"modes":["light","app","partition"],"app_id":1,"genesis_hash":"{GENESIS_HASH}","network":"{NETWORK}","blocks":{{"latest":30,"available":{{"first":20,"last":29}},"app_data":{{"first":20,"last":29}},"historical_sync":{{"synced":false,"available":{{"first":10,"last":19}},"app_data":{{"first":10,"last":18}}}}}},"partition":"1/10"}}"#
		);
		assert_eq!(response.body(), &expected);
	}

	fn all_topics() -> HashSet<Topics> {
		vec![
			Topics::HeaderVerified,
			Topics::ConfidenceAchieved,
			Topics::DataVerified,
		]
		.into_iter()
		.collect()
	}

	fn all_data_fields() -> HashSet<DataFields> {
		vec![DataFields::Extrinsic, DataFields::Data]
			.into_iter()
			.collect()
	}

	#[derive(Clone)]
	struct MockSubmitter {
		pub has_signer: bool,
	}

	#[async_trait]
	impl transactions::Submit for MockSubmitter {
		async fn submit(&self, _: Transaction) -> anyhow::Result<SubmitResponse> {
			Ok(SubmitResponse {
				block_hash: H256::random(),
				hash: H256::random(),
				index: 0,
			})
		}

		fn has_signer(&self) -> bool {
			self.has_signer
		}
	}

	#[test_case(r#"{"raw":""}"#, b"Request body deserialize error: unknown variant `raw`" ; "Invalid json schema")]
	#[test_case(r#"{"data":"dHJhbnooNhY3Rpb24:"}"#, b"Request body deserialize error: Invalid byte" ; "Invalid base64 value")]
	#[tokio::test]
	async fn submit_route_bad_request(json: &str, message: &[u8]) {
		let route = super::submit_route(Some(Arc::new(MockSubmitter { has_signer: true })));
		let response = warp::test::request()
			.method("POST")
			.path("/v2/submit")
			.body(json)
			.reply(&route)
			.await;
		assert_eq!(response.status(), StatusCode::BAD_REQUEST);
		assert!(response.body().starts_with(message));
	}

	#[tokio::test]
	async fn submit_route_no_signign_key() {
		let route = super::submit_route(Some(Arc::new(MockSubmitter { has_signer: false })));
		let response = warp::test::request()
			.method("POST")
			.path("/v2/submit")
			.body(r#"{"data":"dHJhbnNhY3Rpb24K"}"#)
			.reply(&route)
			.await;
		assert_eq!(response.status(), StatusCode::NOT_FOUND);
	}

	#[test_case(r#"{"data":"dHJhbnNhY3Rpb24K"}"# ; "No errors in case of submitted data")]
	#[test_case(r#"{"extrinsic":"dHJhbnNhY3Rpb24K"}"# ; "No errors in case of submitted extrinsic")]
	#[tokio::test]
	async fn submit_route_extrinsic(body: &str) {
		let route = super::submit_route(Some(Arc::new(MockSubmitter { has_signer: true })));
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
		let clients: Clients = Arc::new(RwLock::new(HashMap::new()));
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

		let clients = clients.read().await;
		let client = clients.get(&subscription_id).unwrap();
		let expected = Subscription {
			topics: all_topics(),
			data_fields: all_data_fields(),
		};
		assert!(client.subscription == expected);
	}

	struct MockSetup {
		ws_client: WsClient,
		state: Arc<Mutex<State>>,
	}

	impl MockSetup {
		async fn new(config: RuntimeConfig, submitter: Option<MockSubmitter>) -> Self {
			let client_uuid = uuid::Uuid::new_v4().to_string();
			let client = Client::new(Subscription {
				topics: HashSet::new(),
				data_fields: HashSet::new(),
			});

			let clients = Arc::new(RwLock::new(
				[(client_uuid.clone(), client)]
					.into_iter()
					.collect::<HashMap<_, _>>(),
			));

			let state = Arc::new(Mutex::new(State::default()));
			let route = super::ws_route(
				clients.clone(),
				v1(),
				config.clone(),
				Node::default(),
				submitter.map(Arc::new),
				state.clone(),
			);
			let ws_client = warp::test::ws()
				.path(&format!("/v2/ws/{client_uuid}"))
				.handshake(route)
				.await
				.expect("handshake");

			MockSetup { ws_client, state }
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

		{
			let mut state = test.state.lock().unwrap();
			state.latest = 30;
			state.set_confidence_achieved(20);
			state.set_confidence_achieved(29);
			state.set_data_verified(20);
			state.set_data_verified(29);
			state.set_synced(false);
			state.set_sync_confidence_achieved(10);
			state.set_sync_confidence_achieved(19);
			state.set_sync_data_verified(10);
			state.set_sync_data_verified(18);
		}
		let expected = format!(
			r#"{{"topic":"status","request_id":"363c71fc-90f7-4276-a5b6-bec688bf01e2","message":{{"modes":["light","app","partition"],"app_id":1,"genesis_hash":"{GENESIS_HASH}","network":"{NETWORK}","blocks":{{"latest":30,"available":{{"first":20,"last":29}},"app_data":{{"first":20,"last":29}},"historical_sync":{{"synced":false,"available":{{"first":10,"last":19}},"app_data":{{"first":10,"last":18}}}}}},"partition":"1/10"}}}}"#
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

	#[test_case(r#"{"type":"submit","request_id":"16b24956-2e01-4ba8-bad5-456c561c87d7","message":{"data":""}}"#, None, Some("16b24956-2e01-4ba8-bad5-456c561c87d7"), "Submit is not configured" ; "No submitter")]
	#[test_case(r#"{"type":"submit","request_id":"537a3c39-c029-4283-9612-17465bf7cfd1","message":{"data":"dHJhbnNhY3Rpb24K"}}"#, Some(false), Some("537a3c39-c029-4283-9612-17465bf7cfd1"), "Signer is not configured" ; "No signer")]
	#[test_case(r#"{"type":"submit","request_id":"36bc1f28-e093-422f-964b-1cb1b3882baf","message":{"extrinsic":""}}"#, Some(false), Some("36bc1f28-e093-422f-964b-1cb1b3882baf"), "Transaction is empty" ; "Empty extrinsic")]
	#[test_case(r#"{"type":"submit","request_id":"cc60b2f3-d9ff-4c73-9632-d21d07f7b620","message":{"data":""}}"#, Some(true), Some("cc60b2f3-d9ff-4c73-9632-d21d07f7b620"), "Transaction is empty" ; "Empty data")]
	#[test_case(r#"{"type":"submit","request_id":"9181df86-22f0-42a1-a965-60adb9fc6bdc","message":{"extrinsic":"bad"}}"#, Some(false), None, "Failed to parse request" ; "Bad extrinsic")]
	#[test_case(r#"{"type":"submit","request_id":"78cd7b7b-ba70-48e9-a1da-96b370db4d8f","message":{"data":"bad"}}"#, Some(true), None, "Failed to parse request" ; "Bad data")]
	#[tokio::test]
	async fn ws_route_submit_bad_requests(
		request: &str,
		signer: Option<bool>,
		expected_request_id: Option<&str>,
		expected: &str,
	) {
		let submitter = signer.map(|has_signer| MockSubmitter { has_signer });
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
		let submitter = Some(MockSubmitter { has_signer: true });
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
		let submitter = Some(MockSubmitter { has_signer: true });
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
