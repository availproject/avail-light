extern crate futures;
extern crate num_cpus;

use std::{
	collections::HashSet,
	sync::{Arc, Mutex},
	time::SystemTime,
};

use anyhow::{anyhow, Context, Result};
use futures::stream::{self, StreamExt};
use hyper_tls::HttpsConnector;
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

pub fn generate_random_cells(max_rows: u16, max_cols: u16, block: u64) -> Vec<Cell> {
	let max_cells = (max_rows as u32) * (max_cols as u32);
	let count: u16 = if max_cells < 8 {
		// Multiplication cannot overflow since result is less than 8
		max_rows * max_cols
	} else {
		8
	};
	let mut rng = thread_rng();
	let mut indices = HashSet::new();
	while (indices.len() as u16) < count {
		let row = rng.gen::<u16>() % max_rows;
		let col = rng.gen::<u16>() % max_cols;
		indices.insert(MatrixCell { row, col });
	}

	let mut buf = Vec::new();
	for index in indices {
		buf.push(Cell {
			block,
			row: index.row,
			col: index.col,
			..Default::default()
		});
	}
	buf
}

pub fn generate_kate_query_payload(block: u64, cells: &Vec<Cell>) -> String {
	let mut query = Vec::new();
	for cell in cells {
		query.push(format!(r#"{{"row": {}, "col": {}}}"#, cell.row, cell.col));
	}
	format!(
		r#"{{"id": 1, "jsonrpc": "2.0", "method": "kate_queryProof", "params": [{}, [{}]]}}"#,
		block,
		query.join(", ")
	)
}

// Get proof of certain cell for given block, from full node
pub async fn get_kate_query_proof_by_cell(
	url: &str,
	block: u64,
	row: u16,
	col: u16,
) -> Result<Vec<u8>, String> {
	let payload: String = format!(
		r#"{{"id": 1, "jsonrpc": "2.0", "method": "kate_queryProof", "params": [{}, [{}]]}}"#,
		block,
		format!(r#"{{"row": {}, "col": {}}}"#, row, col)
	);

	match hyper::Request::builder()
		.method(hyper::Method::POST)
		.uri(url)
		.header("Content-Type", "application/json")
		.body(hyper::Body::from(payload))
	{
		Ok(req) => {
			let resp = if is_secure(url) {
				let https = HttpsConnector::new();
				let client = hyper::Client::builder().build::<_, hyper::Body>(https);
				match client.request(req).await {
					Ok(resp) => Some(resp),
					Err(_) => None,
				}
			} else {
				let client = hyper::Client::new();
				match client.request(req).await {
					Ok(resp) => Some(resp),
					Err(_) => None,
				}
			};

			match resp {
				Some(resp) => {
					if let Ok(body) = hyper::body::to_bytes(resp.into_body()).await {
						let r: BlockProofResponse = serde_json::from_slice(&body).unwrap();
						Ok(r.result)
					} else {
						Err("failed to read HTTP POST response".to_owned())
					}
				},
				None => Err("failed to send HTTP POST request".to_owned()),
			}
		},
		Err(_) => Err("failed to build HTTP POST request object".to_owned()),
	}
}

/// For a given block number, retrieves all cell contents by concurrently performing
/// kate proof query RPC and finally returns back one vector of length M x N, when
/// data matrix has M -many rows and N -many columns.
///
/// Each element of resulting vector either has cell content or has nothing ( represented as None )
pub async fn get_cells(
	url: &str,
	msg: &ClientMsg,
	cells: &[(usize, usize)],
) -> Result<Vec<Option<Vec<u8>>>, String> {
	let begin = SystemTime::now();

	let max_cells = (msg.max_rows as usize) * (msg.max_cols as usize);
	let store: Arc<Mutex<Vec<Option<Vec<u8>>>>> = Arc::new(Mutex::new(vec![None; max_cells]));

	let store_0 = store.clone();
	let cells_and_store = cells
		.iter()
		.map(move |(row, col)| (*row, *col, store_0.clone()));

	stream::iter(cells_and_store)
		.for_each_concurrent(num_cpus::get(), |(row, col, store)| async move {
			let proof = get_kate_query_proof_by_cell(url, msg.num, row as u16, col as u16).await;

			let mut handle = store.lock().unwrap();
			handle[col * (msg.max_rows as usize) + row] = match proof {
				Ok(v) => Some(v),
				Err(e) => {
					log::info!("error: {}", e);
					None
				},
			}
		})
		.await;

	log::info!(
		"Received {} cells of block {}\t{:?}",
		max_cells,
		msg.num,
		begin.elapsed().unwrap()
	);

	Arc::try_unwrap(store)
		.map_err(|_| "Failed to unwrap Arc".to_owned())
		.map(|lock| lock.into_inner())
		.and_then(|inner| inner.map_err(|_| "Failed to unwrap Mutex".to_owned()))
}

pub async fn get_kate_proof(url: &str, block_num: u64, mut cells: Vec<Cell>) -> Result<Vec<Cell>> {
	let block = get_block_by_number(url, block_num).await?;

	//tuple of values (id,index)
	let index_tuple = block.header.app_data_lookup.index.clone();

	log::info!(
		"Getting kate proof block {}, apps index {:?}",
		block_num,
		index_tuple
	);

	let payload = generate_kate_query_payload(block_num, &cells);

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

	let proofs_by_cell = proofs.by_cell(cells.len());

	cells
		.iter_mut()
		.zip(proofs_by_cell)
		.for_each(|(cell, proof)| cell.proof = proof.to_vec());

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

fn from_kate_cell(block: u64, cell: &kate_recovery::com::Cell) -> Cell {
	Cell {
		block,
		row: cell.row,
		col: cell.col,
		proof: cell.data.to_vec(),
	}
}

pub fn from_kate_cells(block: u64, cells: &[kate_recovery::com::Cell]) -> Vec<Cell> {
	cells
		.iter()
		.map(|cell| from_kate_cell(block, cell))
		.collect::<Vec<Cell>>()
}
