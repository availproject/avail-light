use std::collections::HashSet;

use anyhow::{anyhow, Context, Result};
use hyper_tls::HttpsConnector;
use kate_recovery::com::{Cell, Position};
use rand::{thread_rng, Rng};
use regex::Regex;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, info};

use crate::types::*;

fn is_secure(url: &str) -> bool {
	let re = Regex::new(r"^https://.*").expect("valid regex");
	re.is_match(url)
}

pub async fn get_blockhash(url: &str, block: u64) -> Result<String> {
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

pub async fn get_block_by_hash(url: &str, hash: String) -> Result<Block> {
	let payload = format!(
		r#"{{"id": 1, "jsonrpc": "2.0", "method": "chain_getBlock", "params": ["{}"]}}"#,
		hash
	);

	let req = hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload))
		.context("Failed to build chain_getBlock request")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("Failed to send chain_getBlock response")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("Failed to get chain_getBlock response")?;
	serde_json::from_slice::<BlockResponse>(&body)
		.map(|r| r.result.block)
		.context("Failed to parse chain_getBlock response")
}

// RPC for obtaining header of latest block mined by network
//
// I'm writing this function so that I can check what's latest block number of chain
// and start syncer to fetch block headers for block range [0, LATEST]
pub async fn get_chain_header(url: &str) -> Result<Header> {
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

pub async fn get_block_by_number(url: &str, block: u64) -> Result<Block> {
	match get_blockhash(url, block).await {
		Ok(hash) => get_block_by_hash(url, hash).await,
		Err(msg) => Err(msg),
	}
}

pub fn generate_random_cells(max_rows: u16, max_cols: u16, cell_count: u32) -> Vec<Position> {
	let max_cells = (max_rows as u32) * (max_cols as u32);
	let count: u16 = if max_cells < cell_count as u32 {
		debug!(
			"Max cells count {} is lesser than cell_count {}",
			cell_count, max_cells
		);
		max_rows * max_cols
	} else {
		cell_count as u16
	};
	let mut rng = thread_rng();
	let mut indices = HashSet::new();
	while (indices.len() as u16) < count {
		let row = rng.gen::<u16>() % max_rows;
		let col = rng.gen::<u16>() % max_cols;
		indices.insert(Position { row, col });
	}

	indices.into_iter().collect::<Vec<_>>()
}

pub fn generate_partition_cells(
	partition: &Partition,
	max_rows: u16,
	max_cols: u16,
) -> Vec<Position> {
	let max_cells = (max_rows as u32) * (max_cols as u32);
	let size = (max_cells as f64 / partition.fraction as f64).ceil() as u32;
	let first_cell = size * (partition.number - 1) as u32;
	let last_cell = size * (partition.number as u32);

	(first_cell..last_cell)
		.map(|cell| {
			let col: u16 = cell as u16 / max_rows;
			let row = cell as u16 % max_rows;
			Position { row, col }
		})
		.collect::<Vec<_>>()
}

pub fn generate_kate_query_payload(block: u64, positions: &[Position]) -> String {
	let query = positions
		.iter()
		.map(|position| format!(r#"{{"row": {}, "col": {}}}"#, position.row, position.col))
		.collect::<Vec<_>>()
		.join(", ");

	format!(
		r#"{{"id": 1, "jsonrpc": "2.0", "method": "kate_queryProof", "params": [{}, [{}]]}}"#,
		block, query
	)
}

pub async fn get_kate_proof(
	url: &str,
	block_num: u64,
	positions: Vec<Position>,
) -> Result<Vec<Cell>> {
	let block = get_block_by_number(url, block_num).await?;

	//tuple of values (id,index)
	let index_tuple = block.header.app_data_lookup.index.clone();

	info!(
		"Getting kate proof block {}, apps index {:?}",
		block_num, index_tuple
	);

	let payload = generate_kate_query_payload(block_num, &positions);

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

	let proofs: BlockProofResponse =
		serde_json::from_slice(&body).context("Failed to parse kate_queryProof response")?;

	let cells = positions
		.iter()
		.zip(proofs.by_cell(positions.len()))
		.map(|(position, &content)| Cell {
			position: position.clone(),
			content,
		})
		.collect::<Vec<_>>();

	Ok(cells)
}

//rpc- only for checking the connecting to substrate node
pub async fn get_chain(url: &str) -> Result<String> {
	let payload = r#"{"id": 1, "jsonrpc": "2.0", "method": "system_chain", "params": []}"#;

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

	serde_json::from_slice::<GetChainResponse>(&body)
		.context("Failed to parse system_chain response")
		.map(|r| r.result)
}

//parsing the urls given in the vector of urls
pub fn parse_urls(urls: &[String]) -> Result<Vec<url::Url>> {
	urls.iter()
		.map(|url| url::Url::parse(url))
		.map(|r| r.map_err(|parse_error| anyhow!("Cannot parse URL: {}", parse_error)))
		.collect::<Result<Vec<_>>>()
}

//fn to check the ws url is working properly and return it
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

//fn to check the rpc_url is secure or not and if it is working properly to return
pub async fn check_http(full_node_rpc: &Vec<String>) -> Result<String> {
	for rpc_url in full_node_rpc {
		if (get_chain(rpc_url).await).is_ok() {
			return Ok(rpc_url.to_string());
		}
	}
	Err(anyhow!("No valid node rpc found from given list"))
}

/* @note: fn to take the number of cells needs to get equal to or greater than
the percentage of confidence mentioned in config file */

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
