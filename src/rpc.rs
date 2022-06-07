extern crate futures;
extern crate num_cpus;

use std::collections::HashSet;

use anyhow::{anyhow, Context, Result};
use hyper_tls::HttpsConnector;
use kate_recovery::com::{Cell, Position};
use rand::{thread_rng, Rng};
use regex::Regex;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::types::*;

fn is_secure(url: &str) -> bool {
	let re = Regex::new(r"^https://.*").unwrap();
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
		.context("failed to build HTTP POST request object(get_blockhash)")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("failed to build HTTP POST request object(get_blockhash)")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("failed to build HTTP POST request object(get_blockhash)")?;
	let r: BlockHashResponse = serde_json::from_slice(&body)
		.context("failed to build HTTP POST request object(get_blockhash)")?;
	Ok(r.result)
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
		.context("failed to build HTTP POST request object(get_block_byhash)")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("failed to build HTTP POST request object(get_block_byhash)")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("failed to build HTTP POST request object(get_block_byhash)")?;
	let r: BlockResponse = serde_json::from_slice(&body)
		.context("failed to build HTTP POST request object(get_block_byhash)")?;
	Ok(r.result.block)
}

// RPC for obtaining header of latest block mined by network
//
// I'm writing this function so that I can check what's latest block number of chain
// and start syncer to fetch block headers for block range [0, LATEST]
pub async fn get_chain_header(url: &str) -> Result<Header> {
	let payload = format!(r#"{{"id": 1, "jsonrpc": "2.0", "method": "chain_getHeader"}}"#,);

	let req = hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload))
		.context("failed to build HTTP POST request object(get_chainHeader)")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("failed to build HTTP POST request object(get_chainHeader)")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("failed to build HTTP POST request object(get_chainHeader)")?;
	let r: BlockHeaderResponse = serde_json::from_slice(&body)
		.context("failed to build HTTP POST request object(get_chainHeader)")?;
	Ok(r.result)
}

pub async fn get_block_by_number(url: &str, block: u64) -> Result<Block> {
	match get_blockhash(url, block).await {
		Ok(hash) => get_block_by_hash(url, hash).await,
		Err(msg) => Err(msg),
	}
}

pub fn generate_random_cells(max_rows: u16, max_cols: u16) -> Vec<Position> {
	let count: u16 = if max_rows * max_cols < 8 {
		max_rows * max_cols
	} else {
		8
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

	log::info!(
		"Getting kate proof block {}, apps index {:?}",
		block_num,
		index_tuple
	);

	let payload = generate_kate_query_payload(block_num, &positions);

	let req = hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload.clone()))
		.context("failed to build HTTP POST request object(kate_proof)")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("failed to build HTTP POST request object(kate_proof)")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("failed to build HTTP POST request object(kate_proof)")?;

	let proofs: BlockProofResponse = serde_json::from_slice(&body)
		.context("failed to build HTTP POST request object(kate_proof)")?;

	let proofs_by_cell = proofs.by_cell(positions.len());

	let cells = positions
		.iter()
		.zip(proofs_by_cell)
		.map(|(position, &content)| Cell {
			position: position.clone(),
			content,
		})
		.collect::<Vec<_>>();

	Ok(cells)
}

//rpc- only for checking the connecting to substrate node
pub async fn get_chain(url: &str) -> Result<String> {
	let payload: String =
		format!(r#"{{"id": 1, "jsonrpc": "2.0", "method": "system_chain", "params": []}}"#);

	let req = hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload))
		.context("failed to build HTTP POST request object(get_chain)")?;

	let resp = if is_secure(url) {
		let https = HttpsConnector::new();
		let client = hyper::Client::builder().build::<_, hyper::Body>(https);
		client.request(req).await
	} else {
		let client = hyper::Client::new();
		client.request(req).await
	}
	.context("failed to build HTTP POST request object(get_chain)")?;

	let body = hyper::body::to_bytes(resp.into_body())
		.await
		.context("failed to build HTTP POST request object(get_chain)")?;
	let r: GetChainResponse = serde_json::from_slice(&body)
		.context("failed to build HTTP POST request object(get_chain)")?;
	Ok(r.result)
}

//parsing the urls given in the vector of urls
pub fn parse_urls(urls: Vec<String>) -> Result<Vec<url::Url>> {
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
pub async fn check_http(full_node_rpc: Vec<String>) -> Result<String> {
	let mut rpc_url = String::new();
	for x in full_node_rpc.iter() {
		if let Ok(_v) = get_chain(x).await {
			rpc_url.push_str(x);
			break;
		}
	}
	Ok(rpc_url)
}
