extern crate futures;
extern crate num_cpus;

use std::{
	collections::{HashMap, HashSet},
	sync::{Arc, Mutex},
	time::SystemTime,
};

use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use hyper_tls::HttpsConnector;
use rand::{thread_rng, Rng};
use regex::Regex;

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

	let store_size = (msg.max_rows * msg.max_cols) as usize;
	let store: Arc<Mutex<Vec<Option<Vec<u8>>>>> = Arc::new(Mutex::new(vec![None; store_size]));

	let store_0 = store.clone();
	let cells_and_store = cells
		.iter()
		.map(move |(row, col)| (*row, *col, store_0.clone()));

	stream::iter(cells_and_store)
		.for_each_concurrent(num_cpus::get(), |(row, col, store)| async move {
			let proof = get_kate_query_proof_by_cell(url, msg.num, row as u16, col as u16).await;

			let mut handle = store.lock().unwrap();
			handle[col * msg.max_rows as usize + row] = match proof {
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
		msg.max_cols * msg.max_rows,
		msg.num,
		begin.elapsed().unwrap()
	);

	Arc::try_unwrap(store)
		.map_err(|_| "Failed to unwrap Arc".to_owned())
		.map(|lock| lock.into_inner())
		.and_then(|inner| inner.map_err(|_| "Failed to unwrap Mutex".to_owned()))
}

pub async fn get_kate_proof(
	url: &str,
	block_num: u64,
	max_rows: u16,
	max_cols: u16,
	app_id: u32,
) -> Result<Vec<Cell>> {
	let block = get_block_by_number(url, block_num).await?;

	//tuple of values (id,index)
	let index_tuple = block.header.app_data_lookup.index.clone();

	log::info!(
		"Getting kate proof block {}, apps index {:?}",
		block_num,
		index_tuple
	);

	let mut cells = match index_tuple
		.iter()
		.find(|elem| app_id != 0 && app_id == elem.0)
	{
		None => generate_random_cells(max_rows, max_cols, block_num),
		Some((app_id, offset)) => {
			log::info!(
				"{} chunks for app {} found in block {}",
				offset,
				app_id,
				block_num
			);
			generate_app_specific_cells(*offset, max_cols, block_num, block, *app_id)
		},
	};
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

pub fn generate_app_specific_cells(
	index: u32,
	max_col: u16,
	num: u64,
	block: Block,
	id: u32,
) -> Vec<Cell> {
	let mut buf = Vec::new();
	let hash_ind = get_id_specific_size(block);
	let endsize = hash_ind.get(&id).unwrap();

	for index in index..=*endsize {
		let rows = (index - 1) as u16 / max_col;
		let cols = (index - 1) as u16 % max_col;

		buf.push(Cell {
			block: num,
			row: rows as u16,
			col: cols as u16,
			..Default::default()
		});
	}
	buf
}

pub fn get_id_specific_size(num: Block) -> HashMap<u32, u32> {
	let app_index = num.header.app_data_lookup.index;
	let app_size = num.header.app_data_lookup.size;
	let mut index: HashMap<u32, u32> = HashMap::new();
	for i in 0..app_index.len() {
		if i + 1 == app_index.len() {
			let esize = app_size;
			index.insert(app_index[i].0, esize);
		} else {
			let size = app_index[i + 1].1 - 1;
			index.insert(app_index[i].0, size);
		}
	}
	index
}
