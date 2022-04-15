extern crate futures;
extern crate num_cpus;

// use async_std::net::TcpStream;
// use std::net::TcpStream;
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

pub fn fill_cells_with_proofs(cells: &mut Vec<Cell>, proof: &BlockProofResponse) {
	assert_eq!(80 * cells.len(), proof.result.len());
	for i in 0..cells.len() {
		let mut v = Vec::new();
		v.extend_from_slice(&proof.result[i * 80..i * 80 + 80]);
		cells[i].proof = v;
	}
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
pub async fn get_all_cells(url: &str, msg: &ClientMsg) -> Result<Vec<Option<Vec<u8>>>, String> {
	let store: Arc<Mutex<Vec<Option<Vec<u8>>>>> = Arc::new(Mutex::new(vec![
		None;
		(msg.max_rows * msg.max_cols)
			as usize
	]));

	let begin = SystemTime::now();
	let store_0 = store.clone();
	let fut = stream::iter(0..msg.max_rows as usize)
		.flat_map(|row| stream::iter(0..msg.max_cols as usize).map(move |col| (row, col)))
		.zip(stream::iter(0..(msg.max_rows * msg.max_cols) as usize).map(move |_| store_0.clone()))
		.for_each_concurrent(num_cpus::get(), |((row, col), store)| async move {
			match get_kate_query_proof_by_cell(url, msg.num, row as u16, col as u16).await {
				Ok(v) => {
					let mut handle = store.lock().unwrap();
					handle[row * msg.max_cols as usize + col] = Some(v);
				},
				Err(e) => {
					let mut handle = store.lock().unwrap();
					handle[row * msg.max_cols as usize + col] = None;
					log::info!("error: {}", e)
				},
			}
		});

	fut.await;
	log::info!(
		"Received {} cells of block {}\t{:?}",
		msg.max_cols * msg.max_rows,
		msg.num,
		begin.elapsed().unwrap()
	);

	match Arc::try_unwrap(store) {
		Ok(lock) => match lock.into_inner() {
			Ok(v) => Ok(v),
			Err(_) => Err("failed to unwrap Mutex".to_owned()),
		},
		Err(_) => Err("failed to unwrap Arc".to_owned()),
	}
}

pub async fn get_kate_proof(
	url: &str,
	block: u64,
	max_rows: u16,
	max_cols: u16,
	app_id: u32,
) -> Result<Vec<Cell>> {
	let num = get_block_by_number(url, block).await?;

	//tuple of values (id,index)
	let index_tuple = num.header.app_data_lookup.index.clone();

	log::info!(
		"Getting kate proof block {}, apps index {:?}",
		block,
		index_tuple
	);

	//checking for if the user is subscribed for a particular APPID
	let mut cells = if app_id == 0 || index_tuple.iter().all(|elem| app_id != elem.0) {
		generate_random_cells(max_rows, max_cols, block)
	} else {
		//this is where the index for a specific app_ID is checked; from the tuple (id, index).
		let mut app_ind: u32 = 0;
		for i in 0..index_tuple.len() {
			if app_id == index_tuple[i].0 {
				app_ind = index_tuple[i].1;
				break;
			}
		}
		log::info!(
			"{} chunks for app {} found in block {}",
			app_ind,
			app_id,
			block
		);
		generate_app_specific_cells(app_ind, max_cols, block, num, app_id)
	};
	let payload = generate_kate_query_payload(block, &cells);

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

	let r: BlockProofResponse = serde_json::from_slice(&body)
		.context("failed to build HTTP POST request object(kate_proof)")?;

	fill_cells_with_proofs(&mut cells, &r);
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

pub async fn check_connection(
	full_node_ws: Vec<String>,
) -> Result<Option<WebSocketStream<MaybeTlsStream<TcpStream>>>> {
	let mut ws_ = None;
	for x in full_node_ws.iter() {
		let _url = url::Url::parse(&x)?;
		if let Ok(v) = connect_async(_url).await {
				let (ws__, _) = v;
				ws_ = Some(ws__);
				break;
			};
	}
	Ok(ws_)
}

pub async fn check_http(full_node_rpc:Vec<String>) -> Result<String> {
	println!("testing the check_http");
	let mut rpc_url= String::new(); 
	for x in full_node_rpc.iter(){
		println!("testing x{:?}",x);
		let url_ = x.parse::<hyper::Uri>().context("http url parse failed")?;
		println!("testing url {:?}",url_);
		if is_secure(x) {
			println!("testing the is_secure");
			let https = HttpsConnector::new();
			let client = hyper::Client::builder().build::<_, hyper::Body>(https);
			let res = match client.get(url_).await{
				Ok(c) => c,
				Err(e) => {
					println!("error in secure http");
					continue;
				},
			};
			if res.status().is_success() {
				println!("testing res in https");
				rpc_url.push_str(x);
				break;
			}				
		}else{
			println!("testing the block");
			let client = hyper::Client::new();	
			println!("testing after client is build");
			let _res = match client.get(url_).await{
				Ok(c) => c,
				Err(e) => {
					println!("error in else http");
					continue;
				},
			};
			println!("res status {:?}",_res.status());
			//@TODO: need to find an alternative way for http part
			// if _res.status().is_success() {
				// if let Ok(v) = get_chain_header(x).await {
					println!("testing in else {:?}",x);
					rpc_url.push_str(x);
					// break;
				
		}	
	}
	println!("💡 testing rpc_URL {:?}",rpc_url.clone());
	Ok(rpc_url)
}
