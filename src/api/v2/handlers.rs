use super::types::{Client, Clients, Subscription, SubscriptionId, Version};
use std::convert::Infallible;
use uuid::Uuid;

pub fn version(version: String, network_version: String) -> Version {
	Version {
		version,
		network_version,
	}
}

pub async fn subscriptions(
	subscription: Subscription,
	clients: Clients,
) -> Result<SubscriptionId, Infallible> {
	let subscription_id = Uuid::new_v4().to_string();
	let mut clients = clients.write().await;
	clients.insert(subscription_id.clone(), Client { subscription });
	Ok(SubscriptionId { subscription_id })
}
