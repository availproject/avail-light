use actix_web::{get, web, HttpResponse, Responder};
use libp2p::PeerId;
use serde::{Deserialize, Serialize};
use std::{
	collections::HashMap,
	sync::Arc,
	time::{Duration, SystemTime},
};
use tokio::sync::Mutex;

use crate::{config::PaginationConfig, types::ServerInfo};

#[derive(Serialize, Deserialize, Debug)]
pub struct PeerInfo {
	peer_id: String,
	multiaddr: Vec<String>,
	last_discovered: Option<u64>,
	last_successful_dial: Option<u64>,
	average_ping_ms: Option<u128>,
	last_ping_rtt: Option<Duration>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PeerCounts {
	server_list_count: usize,
	blacklist_count: usize,
}
#[derive(Serialize, Deserialize, Debug)]
pub struct PaginatedResponse<T> {
	total_items: usize,
	total_pages: usize,
	current_page: usize,
	page_size: usize,
	next_page: Option<String>,
	prev_page: Option<String>,
	data: T,
}

#[derive(Deserialize)]
pub struct PaginationParams {
	#[serde(default = "default_page")]
	pub page: usize,
	#[serde(default = "default_limit")]
	pub limit: usize,
}

fn default_page() -> usize {
	1
}

fn default_limit() -> usize {
	50
}

pub struct AppState {
	pub blacklist: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	pub server_list: Arc<Mutex<HashMap<PeerId, ServerInfo>>>,
	pub pagination: PaginationConfig,
}

#[get("/blacklisted_peers")]
async fn get_blacklisted_peers(
	app_state: web::Data<AppState>,
	pagination: web::Query<PaginationParams>,
) -> impl Responder {
	let blacklist = app_state.blacklist.lock().await;

	let page = app_state.pagination.validate_page(pagination.page);
	let limit = app_state.pagination.validate_limit(pagination.limit);

	let peers: Vec<(String, PeerInfo)> = blacklist
		.iter()
		.map(|(peer_id, info)| {
			let peer_id_str = peer_id.to_string();

			let peer_info = PeerInfo {
				peer_id: peer_id_str.clone(),
				multiaddr: info.multiaddr.iter().map(|addr| addr.to_string()).collect(),
				last_discovered: info.last_discovered.map(|time| {
					time.duration_since(SystemTime::UNIX_EPOCH)
						.unwrap_or_default()
						.as_secs()
				}),
				last_successful_dial: info.last_successful_dial.map(|time| {
					time.duration_since(SystemTime::UNIX_EPOCH)
						.unwrap_or_default()
						.as_secs()
				}),
				last_ping_rtt: None,
				average_ping_ms: None,
			};

			(peer_id_str, peer_info)
		})
		.collect();

	let mut sorted_peers = peers;
	sorted_peers.sort_by(|a, b| a.0.cmp(&b.0));

	let response = paginate_as_array(sorted_peers, page, limit, "/blacklisted_peers");

	HttpResponse::Ok().json(response)
}

#[get("/peers")]
async fn get_peers(
	app_state: web::Data<AppState>,
	pagination: web::Query<PaginationParams>,
) -> impl Responder {
	let page = app_state.pagination.validate_page(pagination.page);
	let limit = app_state.pagination.validate_limit(pagination.limit);

	let server_list = app_state.server_list.lock().await;
	let blacklist = app_state.blacklist.lock().await;

	let server_peers: Vec<(String, PeerInfo)> = server_list
		.iter()
		.map(|(peer_id, info)| {
			let is_blacklisted = blacklist.contains_key(peer_id);
			let (avg_ping_ms, last_ping) = if is_blacklisted {
				(None, None)
			} else {
				(info.avg_ping().map(|d| d.as_millis()), info.last_ping_rtt)
			};
			(
				peer_id.to_string(),
				PeerInfo {
					peer_id: peer_id.to_string(),
					multiaddr: info.multiaddr.iter().map(|addr| addr.to_string()).collect(),
					last_discovered: info.last_discovered.map(|time| {
						time.duration_since(SystemTime::UNIX_EPOCH)
							.unwrap_or_default()
							.as_secs()
					}),
					last_successful_dial: info.last_successful_dial.map(|time| {
						time.duration_since(SystemTime::UNIX_EPOCH)
							.unwrap_or_default()
							.as_secs()
					}),
					average_ping_ms: avg_ping_ms,
					last_ping_rtt: last_ping,
				},
			)
		})
		.collect();
	let mut sorted_peers = server_peers;
	sorted_peers.sort_by(|a, b| a.0.cmp(&b.0));

	let response = paginate_as_array(sorted_peers, page, limit, "/peers");
	HttpResponse::Ok().json(response)
}

#[get("/peer_count")]
async fn get_peer_count(app_state: web::Data<AppState>) -> impl Responder {
	let server_list = app_state.server_list.lock().await;
	let blacklist = app_state.blacklist.lock().await;

	let counts = PeerCounts {
		server_list_count: server_list.len(),
		blacklist_count: blacklist.len(),
	};

	HttpResponse::Ok().json(counts)
}

fn build_pagination_urls(
	endpoint: &str,
	page: usize,
	limit: usize,
	total_pages: usize,
) -> (Option<String>, Option<String>) {
	let next_page = if page < total_pages {
		Some(format!("{}?page={}&limit={}", endpoint, page + 1, limit))
	} else {
		None
	};

	let prev_page = if page > 1 {
		Some(format!("{}?page={}&limit={}", endpoint, page - 1, limit))
	} else {
		None
	};

	(next_page, prev_page)
}

fn paginate_as_array<T>(
	items: Vec<(String, T)>,
	page: usize,
	limit: usize,
	endpoint: &str,
) -> PaginatedResponse<Vec<T>>
where
	T: Serialize,
{
	let total_items = items.len();
	let total_pages = total_items.div_ceil(limit);
	let start_index = (page - 1) * limit;

	let (next_page, prev_page) = build_pagination_urls(endpoint, page, limit, total_pages);

	let data: Vec<T> = items
		.into_iter()
		.skip(start_index)
		.take(limit)
		.map(|(_, value)| value)
		.collect();

	PaginatedResponse {
		total_items,
		total_pages,
		current_page: page,
		page_size: limit,
		next_page,
		prev_page,
		data,
	}
}
