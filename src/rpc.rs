//! RPC communication with avail node.

use std::collections::HashSet;

use anyhow::{anyhow, Context, Result};
use avail_subxt::primitives::Header as DaHeader;
use codec::{Decode, Encode};
use hyper_tls::HttpsConnector;
use kate_recovery::{
	data::Cell,
	matrix::{Dimensions, Position},
};
use rand::{thread_rng, Rng};
use regex::Regex;
use sp_core::H256;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, instrument};

use crate::types::*;

fn is_secure(url: &str) -> bool {
	let re = Regex::new(r"^https://.*").expect("valid regex");
	re.is_match(url)
}

async fn get_block_hash(url: &str, block: u32) -> Result<String> {
	let payload = format!(
		r#"{{"id": 1, "jsonrpc": "2.0", "method": "chain_getBlockHash", "params": [{}]}}"#,
		block
	);

	let req = hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload))
		.context("Failed to build chain_getBlockHash request")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("Failed to send chain_getBlockHash request")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("Failed to get chain_getBlockHash response")?;

	serde_json::from_slice::<BlockHashResponse>(&body)
		.map(|r| r.result)
		.context("Failed to parse chain_getBlockHash response")
}

async fn get_header_by_hash(url: &str, hash: &str) -> Result<DaHeader> {
	let payload = format!(
		r#"{{"id": 1, "jsonrpc": "2.0", "method": "chain_getHeader", "params": ["{}"]}}"#,
		hash
	);

	let req = hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload))
		.context("Failed to build chain_getHeader request")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("Failed to send chain_getHeader response")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("Failed to get chain_getHeader response")?;

	serde_json::from_slice::<BlockHeaderResponse>(&body)
		.map(|r| r.result)
		.context("Failed to parse chain_getHeader response")
}

/// RPC for obtaining header of latest block mined by network
// I'm writing this function so that I can check what's latest block number of chain
// and start syncer to fetch block headers for block range [0, LATEST]
pub async fn get_chain_header(url: &str) -> Result<DaHeader> {
	let payload = r#"{"id": 1, "jsonrpc": "2.0", "method": "chain_getHeader"}"#;

	let req = hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload))
		.context("Failed to build chain_getHeader request")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("Failed to send chain_getHeader request")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("Failed to get chain_getHeader response")?;

	serde_json::from_slice::<BlockHeaderResponse>(&body)
		.map(|r| r.result)
		.context("Failed to parse chain_getHeader response")
}

/// Gets header by block number
pub async fn get_header_by_block_number(url: &str, block: u32) -> Result<(DaHeader, H256)> {
	let hash = get_block_hash(url, block).await?;
	let v = sp_core::bytes::from_hex(&hash).context("Wrong hash format")?;
	let h: H256 = Decode::decode(&mut v.as_slice()).context("Cannot decode hash")?;
	get_header_by_hash(url, hash.as_str()).await.map(|e| (e, h))
}

/// Generates random cell positions for sampling
pub fn generate_random_cells(dimensions: &Dimensions, cell_count: u32) -> Vec<Position> {
	let max_cells = dimensions.extended_size();
	let count = if max_cells < cell_count {
		debug!("Max cells count {max_cells} is lesser than cell_count {cell_count}");
		max_cells
	} else {
		cell_count
	};
	let mut rng = thread_rng();
	let mut indices = HashSet::new();
	while (indices.len() as u16) < count as u16 {
		let row = rng.gen::<u32>() % dimensions.extended_rows();
		let col = rng.gen::<u16>() % dimensions.cols;
		indices.insert(Position { row, col });
	}

	indices.into_iter().collect::<Vec<_>>()
}

fn generate_kate_query_payload(block_hash: H256, positions: &[Position]) -> String {
	let query = positions
		.iter()
		.map(|position| format!(r#"{{"row": {}, "col": {}}}"#, position.row, position.col))
		.collect::<Vec<_>>()
		.join(", ");
	let hash = sp_core::bytes::to_hex(block_hash.encode().as_slice(), false);

	format!(
		r#"{{"id": 1, "jsonrpc": "2.0", "method": "kate_queryProof", "params": [[{}], "{}"]}}"#,
		query, hash
	)
}

#[instrument(skip_all, level = "trace")]
pub async fn get_kate_app_data(
	url: &str,
	block_hash: H256,
	app_id: u32,
) -> Result<Vec<Option<Vec<u8>>>> {
	let payload = format!(
		r#"{{"id": 1, "jsonrpc": "2.0", "method": "kate_queryAppData", "params": [{}, "{:?}"]}}"#,
		app_id, block_hash
	);

	let request = hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload))
		.context("Failed to build kate_queryAppData request")?;

	let response = if is_secure(url) {
		let client = hyper::Client::builder().build::<_, hyper::Body>(HttpsConnector::new());
		client.request(request).await
	} else {
		hyper::Client::new().request(request).await
	}
	.context("Failed to send kate_queryAppData request")?;

	let body = hyper::body::to_bytes(response.into_body())
		.await
		.context("Failed to get kate_queryAppData response")?;

	let response: QueryAppDataResponse =
		serde_json::from_slice(&body).context("Failed to parse kate_queryAppData response")?;

	match response {
		QueryAppDataResponse::Block(rows) => Ok(rows.result),
		QueryAppDataResponse::Error(error) => Err(anyhow!(error.message())),
	}
}

/// RPC to get proofs for given positions of block
pub async fn get_kate_proof(
	url: &str,
	block_hash: H256,
	positions: Vec<Position>,
) -> Result<Vec<Cell>> {
	let payload = generate_kate_query_payload(block_hash, &positions);

	let req = hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload.clone()))
		.context("Failed to build kate_queryProof request")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("Failed to send kate_queryProof request")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("Failed to get kate_queryProof response")?;

	let response: QueryProofResponse =
		serde_json::from_slice(&body).context("Failed to parse kate_queryProof response")?;

	match response {
		QueryProofResponse::Proofs(proofs) => Ok(positions
			.iter()
			.zip(proofs.by_cell(positions.len()))
			.map(|(position, &content)| Cell {
				position: position.clone(),
				content,
			})
			.collect::<Vec<_>>()),
		QueryProofResponse::Error(error) => Err(anyhow!(error.message())),
	}
}

// RPC to check connection to substrate node
pub async fn get_system_version(url: &str) -> Result<String> {
	let payload = r#"{"id": 1, "jsonrpc": "2.0", "method": "system_version", "params": []}"#;

	let req = hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload))
		.context("Failed to build system_chain request")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("Failed to send system_chain request")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("Failed to get system_chain response")?;

	serde_json::from_slice::<SystemVersionResponse>(&body)
		.context("Failed to parse system_chain response")
		.map(|r| r.result)
}

pub async fn get_runtime_version(url: &str) -> Result<RuntimeVersionResult> {
	let payload =
		r#"{"id": 1, "jsonrpc": "2.0", "method": "chain_getRuntimeVersion", "params": []}"#;

	let req = hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload))
		.context("Failed to build system_chain request")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("Failed to send system_chain request")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("Failed to get system_chain response")?;

	serde_json::from_slice::<RuntimeVersionResponse>(&body)
		.context("Failed to parse chain_getRuntimeVersion response")
		.map(|r| r.result)
}

/// Parsing the urls given in the vector of urls
pub fn parse_urls(urls: &[String]) -> Result<Vec<url::Url>> {
	urls.iter()
		.map(|url| url::Url::parse(url))
		.map(|r| r.map_err(|parse_error| anyhow!("Cannot parse URL: {}", parse_error)))
		.collect::<Result<Vec<_>>>()
}

/// Checks the WS urls and returns first working
pub async fn check_connection(
	full_node_ws: &[url::Url],
) -> Option<WebSocketStream<MaybeTlsStream<TcpStream>>> {
	// TODO: We are ignoring errors here, we should probably return result instead of option
	for url in full_node_ws.iter() {
		if let Ok((ws, _)) = connect_async(url).await {
			return Some(ws);
		};
	}
	None
}

/// Checks if the rpc_url is secure or not and if it is working properly to return
pub async fn check_http(full_node_rpc: &Vec<String>) -> Result<String> {
	for rpc_url in full_node_rpc {
		if (get_system_version(rpc_url).await).is_ok() {
			return Ok(rpc_url.to_string());
		}
	}
	Err(anyhow!("No valid node rpc found from given list"))
}

/* @note: fn to take the number of cells needs to get equal to or greater than
the percentage of confidence mentioned in config file */

/// Callculates number of cells required to achieve given confidence
pub fn cell_count_for_confidence(confidence: f64) -> u32 {
	let mut cell_count: u32;
	if (confidence >= 100f64) || (confidence < 50.0) {
		//in this default of 8 cells will be taken
		debug!(
			"confidence is {} invalid so taking default confidence of 99",
			confidence
		);
		cell_count = (-((1f64 - (99f64 / 100f64)).log2())).ceil() as u32;
	} else {
		cell_count = (-((1f64 - (confidence / 100f64)).log2())).ceil() as u32;
	}
	if cell_count == 0 || cell_count > 10 {
		debug!(
			"confidence is {} invalid so taking default confidence of 99",
			confidence
		);
		cell_count = (-((1f64 - (99f64 / 100f64)).log2())).ceil() as u32;
	}
	cell_count
}
