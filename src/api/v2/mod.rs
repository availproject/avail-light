use crate::{
	rpc::Node,
	types::{RuntimeConfig, State},
};

use self::{
	handlers::handle_rejection,
	types::{Clients, Version},
};
use std::{
	collections::HashMap,
	convert::Infallible,
	sync::{Arc, Mutex},
};
use tokio::sync::RwLock;
use warp::{Filter, Rejection, Reply};

mod handlers;
mod types;
mod ws;

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
		.then(handlers::status)
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
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	warp::path!("v2" / "ws" / String)
		.and(warp::ws())
		.and(with_clients(clients))
		.and(warp::any().map(move || version.clone()))
		.and_then(handlers::ws)
}

pub fn routes(
	version: String,
	network_version: String,
	node: Node,
	state: Arc<Mutex<State>>,
	config: RuntimeConfig,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	let clients: Clients = Arc::new(RwLock::new(HashMap::new()));
	let version = Version {
		version,
		network_version,
	};
	version_route(version.clone())
		.or(status_route(config, node, state))
		.or(subscriptions_route(clients.clone()))
		.or(ws_route(clients, version))
		.recover(handle_rejection)
}

#[cfg(test)]
mod tests {
	use super::types::Client;
	use crate::{
		api::v2::types::{Clients, DataFields, Subscription, SubscriptionId, Topics, Version},
		rpc::Node,
		types::{RuntimeConfig, State},
	};
	use kate_recovery::matrix::Partition;
	use sp_core::H256;
	use std::{
		collections::{HashMap, HashSet},
		str::FromStr,
		sync::{Arc, Mutex},
	};
	use tokio::sync::RwLock;
	use uuid::Uuid;

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
		vec![DataFields::Raw, DataFields::Data]
			.into_iter()
			.collect()
	}

	#[tokio::test]
	async fn subscriptions_route() {
		let clients: Clients = Arc::new(RwLock::new(HashMap::new()));
		let route = super::subscriptions_route(clients.clone());

		let body = r#"{"topics":["confidence-achieved","data-verified","header-verified"],"data_fields":["data","raw"]}"#;
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

	async fn init_clients() -> (Uuid, Clients) {
		let uuid = uuid::Uuid::new_v4();
		let clients: Clients = Arc::new(RwLock::new(HashMap::new()));

		let client = Client::new(Subscription {
			topics: HashSet::new(),
			data_fields: HashSet::new(),
		});
		clients.write().await.insert(uuid.to_string(), client);
		(uuid, clients)
	}

	#[tokio::test]
	async fn ws_route_version() {
		let (uuid, clients) = init_clients().await;

		let route = super::ws_route(clients.clone(), v1());
		let mut client = warp::test::ws()
			.path(&format!("/v2/ws/{uuid}"))
			.handshake(route)
			.await
			.expect("handshake");

		client
			.send_text(r#"{"type":"version","request_id":"1"}"#)
			.await;

		let expected = r#"{"topic":"version","request_id":"1","message":{"version":"v1.0.0","network_version":"nv1.0.0"}}"#;
		let message = client.recv().await.unwrap();
		assert_eq!(expected, message.to_str().unwrap());
	}

	#[tokio::test]
	async fn ws_route_bad_request() {
		let (uuid, clients) = init_clients().await;

		let route = super::ws_route(clients.clone(), v1());
		let mut client = warp::test::ws()
			.path(&format!("/v2/ws/{uuid}"))
			.handshake(route)
			.await
			.expect("handshake");

		client
			.send_text(r#"{"type":"vers1on","request_id":"1"}"#)
			.await;

		let expected = r#"{"error_code":"bad-request","message":"Error handling web socket message: Failed to parse request"}"#;
		let message = client.recv().await.unwrap();
		assert_eq!(expected, message.to_str().unwrap())
	}
}
