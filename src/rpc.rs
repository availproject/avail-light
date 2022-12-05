//! RPC communication with avail node.

use std::{collections::HashSet, ops::Deref};

use anyhow::{anyhow, Result};
use avail_subxt::{primitives::Header as DaHeader, AvailConfig};
use kate_recovery::{
	data::Cell,
	matrix::{Dimensions, Position},
};
use rand::{thread_rng, Rng};
use serde::Serialize;
use sp_core::H256;
use subxt::{
	rpc::{BlockNumber, RpcParams},
	OnlineClient,
};
use tracing::{debug, instrument};

use crate::types::*;

async fn get_block_hash(url: &str, block: u32) -> Result<H256> {
	let client = avail_subxt::build_client(url).await?;
	client
		.rpc()
		.block_hash(Some(BlockNumber::from(block)))
		.await?
		.ok_or(anyhow!("Block with number {block} not found"))
}

async fn get_header_by_hash(url: &str, hash: H256) -> Result<DaHeader> {
	let client = avail_subxt::build_client(url).await?;
	client
		.rpc()
		.header(Some(hash))
		.await?
		.ok_or(anyhow!("Header with hash {hash:?} not found"))
}

/// RPC for obtaining header of latest block mined by network
// I'm writing this function so that I can check what's latest block number of chain
// and start syncer to fetch block headers for block range [0, LATEST]
pub async fn get_chain_header(url: &str) -> Result<DaHeader> {
	let client = avail_subxt::build_client(url).await?;
	client
		.rpc()
		.header(None)
		.await?
		.ok_or(anyhow!("Latest header not found"))
}

/// Gets header by block number
pub async fn get_header_by_block_number(url: &str, block: u32) -> Result<(DaHeader, H256)> {
	let hash = get_block_hash(url, block).await?;
	get_header_by_hash(url, hash).await.map(|e| (e, hash))
}

/// Generates random cell positions for sampling
pub fn generate_random_cells(dimensions: &Dimensions, cell_count: u32) -> Vec<Position> {
	let max_cells = dimensions.extended_size();
	let count = if max_cells < cell_count.into() {
		debug!("Max cells count {max_cells} is lesser than cell_count {cell_count}");
		max_cells
	} else {
		cell_count.into()
	};
	let mut rng = thread_rng();
	let mut indices = HashSet::new();
	while (indices.len() as u16) < count as u16 {
		let row = rng.gen::<u32>() % dimensions.extended_rows();
		let col = rng.gen::<u16>() % dimensions.cols();
		indices.insert(Position { row, col });
	}

	indices.into_iter().collect::<Vec<_>>()
}

#[instrument(skip_all, level = "trace")]
pub async fn get_kate_app_data(
	url: &str,
	block_hash: H256,
	app_id: u32,
) -> Result<Vec<Option<Vec<u8>>>> {
	let client = avail_subxt::build_client(url).await?;

	let mut params = RpcParams::new();
	params.push(app_id)?;
	params.push(block_hash)?;
	let t = client.rpc().deref();
	t.request("kate_queryAppData", params)
		.await
		.map_err(|e| anyhow!("Version couldn't be retrieved, error: {e}"))
}

#[derive(Serialize)]
struct QueryPosition {
	pub row: u32,
	pub col: u16,
}

impl From<Position> for QueryPosition {
	fn from(e: Position) -> Self {
		Self {
			row: e.row,
			col: e.col,
		}
	}
}
/// RPC to get proofs for given positions of block
pub async fn get_kate_proof(
	url: &str,
	block_hash: H256,
	positions: Vec<Position>,
) -> Result<Vec<Cell>> {
	// let payload = generate_kate_query_payload(block_hash, &positions);
	let pos = positions
		.iter()
		.cloned()
		.map(QueryPosition::from)
		.collect::<Vec<_>>();
	let mut params = RpcParams::new();
	params.push(pos)?;
	params.push(block_hash)?;

	let client = avail_subxt::build_client(url).await?;
	let t = client.rpc().deref();
	let proofs: Vec<u8> = t
		.request("kate_queryProof", params)
		.await
		.map_err(|e| anyhow!("Error fetching proof: {e}"))?;

	let i = proofs
		.chunks_exact(CELL_WITH_PROOF_SIZE)
		.map(|chunk| chunk.try_into().expect("chunks of 80 bytes size"));
	Ok(positions
		.iter()
		.zip(i)
		.map(|(position, &content)| Cell {
			position: position.clone(),
			content,
		})
		.collect::<Vec<_>>())
}

// RPC to check connection to substrate node
pub async fn get_system_version(url: &str) -> Result<String> {
	let client = avail_subxt::build_client(url).await?;
	client
		.rpc()
		.system_version()
		.await
		.map_err(|e| anyhow!("Version couldn't be retrieved, error: {e}"))
}

pub async fn get_runtime_version(url: &str) -> Result<RuntimeVersionResult> {
	let client = avail_subxt::build_client(url).await?;
	let t = client.rpc().deref();
	t.request("state_getRuntimeVersion", RpcParams::new())
		.await
		.map_err(|e| anyhow!("Version couldn't be retrieved, error: {e}"))
}

/// Parsing the urls given in the vector of urls
pub fn parse_urls(urls: &[String]) -> Result<Vec<url::Url>> {
	urls.iter()
		.map(|url| url::Url::parse(url))
		.map(|r| r.map_err(|parse_error| anyhow!("Cannot parse URL: {}", parse_error)))
		.collect::<Result<Vec<_>>>()
}

/// Checks the WS urls and returns first working
pub async fn check_connection(full_node_ws: &[url::Url]) -> Option<OnlineClient<AvailConfig>> {
	// TODO: We are ignoring errors here, we should probably return result instead of option
	for url in full_node_ws.iter() {
		if let Ok(client) = avail_subxt::build_client(url.as_str()).await {
			return Some(client);
		};
	}
	None
}

/// Checks if the rpc_url is secure or not and if it is working properly to return
pub async fn check_http(full_node_rpc: &Vec<String>) -> Result<String> {
	for rpc_url in full_node_rpc {
		let ret = get_system_version(rpc_url).await;
		println!("RET={ret:?}");
		if ret.is_ok() {
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
