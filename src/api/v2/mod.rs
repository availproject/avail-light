use self::types::Clients;
use std::{collections::HashMap, convert::Infallible, sync::Arc};
use tokio::sync::RwLock;
use warp::{Filter, Rejection, Reply};

mod handlers;
mod types;

fn with_clients(clients: Clients) -> impl Filter<Extract = (Clients,), Error = Infallible> + Clone {
	warp::any().map(move || clients.clone())
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
	let clients: Clients = Arc::new(RwLock::new(HashMap::new()));
	version_route(version, network_version).or(subscriptions_route(clients))
}

#[cfg(test)]
mod tests {
	use crate::api::v2::types::{Clients, DataFields, Subscription, SubscriptionId, Topics};
	use std::{
		collections::{HashMap, HashSet},
		str::FromStr,
		sync::Arc,
	};
	use tokio::sync::RwLock;

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
}
