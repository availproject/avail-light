use self::types::{Clients, Version};
use std::{collections::HashMap, convert::Infallible, sync::Arc};
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
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
	let clients: Clients = Arc::new(RwLock::new(HashMap::new()));
	let version = Version {
		version,
		network_version,
	};
	version_route(version.clone())
		.or(subscriptions_route(clients.clone()))
		.or(ws_route(clients, version))
}

#[cfg(test)]
mod tests {
	use crate::api::v2::types::{
		Clients, DataFields, Subscription, SubscriptionId, Topics, Version,
	};
	use std::{
		collections::{HashMap, HashSet},
		str::FromStr,
		sync::Arc,
	};
	use tokio::sync::RwLock;
	use uuid::Uuid;

	use super::types::Client;

	fn v1() -> Version {
		Version {
			version: "v1.0.0".to_string(),
			network_version: "nv1.0.0".to_string(),
		}
	}

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
