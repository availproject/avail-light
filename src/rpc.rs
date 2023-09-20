//! RPC communication with avail node.

use anyhow::{anyhow, Context, Result};
use avail_subxt::{
	avail, build_client,
	primitives::Header as DaHeader,
	rpc::{types::BlockNumber, RpcParams},
	utils::H256,
};
use kate_recovery::{
	data::Cell,
	matrix::{Dimensions, Position},
};
use rand::{seq::SliceRandom, thread_rng, Rng};
use sp_core::ed25519;
use std::{collections::HashSet, fmt::Display, ops::Deref};
use tracing::{debug, info, instrument, warn};

use crate::{consts::EXPECTED_NETWORK_VERSION, types::*};

pub async fn get_block_hash(client: &avail::Client, block: u32) -> Result<H256> {
	client
		.rpc()
		.block_hash(Some(BlockNumber::from(block)))
		.await?
		.ok_or_else(|| anyhow!("Block with number {block} not found"))
}

pub async fn get_header_by_hash(client: &avail::Client, hash: H256) -> Result<DaHeader> {
	client
		.rpc()
		.header(Some(hash))
		.await?
		.ok_or_else(|| anyhow!("Header with hash {hash:?} not found"))
}

pub async fn get_valset_by_hash(
	client: &avail::Client,
	hash: H256,
) -> Result<Vec<ed25519::Public>> {
	let grandpa_valset = client
		.runtime_api()
		.at(hash)
		.call_raw::<Vec<(ed25519::Public, u64)>>("GrandpaApi_grandpa_authorities", None)
		.await
		.unwrap();

	// Drop weights, as they are not currently used.
	Ok(grandpa_valset.iter().map(|e| e.0).collect())
}

pub async fn get_valset_by_block_number(
	client: &avail::Client,
	block: u32,
) -> Result<Vec<ed25519::Public>> {
	let hash = get_block_hash(client, block).await?;
	get_valset_by_hash(client, hash).await
}
/// RPC for obtaining header of latest finalized block mined by network
pub async fn get_chain_head_header(client: &avail::Client) -> Result<DaHeader> {
	let h = client.rpc().finalized_head().await?;
	client
		.rpc()
		.header(Some(h))
		.await?
		.context("Couldn't get latest finalized header")
}

pub async fn get_chain_head_hash(client: &avail::Client) -> Result<H256> {
	client
		.rpc()
		.finalized_head()
		.await
		.context("Cannot get finalized head hash")
}

pub async fn get_set_id_by_hash(client: &avail::Client, hash: H256) -> Result<u64> {
	let set_id_key = avail_subxt::api::storage().grandpa().current_set_id();
	// Fetch the set ID from storage at current height
	Ok(client
		.storage()
		// None means current height
		.at(hash)
		.fetch(&set_id_key)
		.await?
		.expect("The set_id should exist"))
}

pub async fn get_set_id_by_block_number(client: &avail::Client, block: u32) -> Result<u64> {
	let hash = get_block_hash(client, block).await?;
	get_set_id_by_hash(client, hash).await
}

/// Gets header by block number
pub async fn get_header_by_block_number(
	client: &avail::Client,
	block: u32,
) -> Result<(DaHeader, H256)> {
	let hash = get_block_hash(client, block).await?;
	get_header_by_hash(client, hash).await.map(|e| (e, hash))
}

/// Generates random cell positions for sampling
pub fn generate_random_cells(dimensions: Dimensions, cell_count: u32) -> Vec<Position> {
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
		let col = rng.gen_range(0..dimensions.cols().into());
		let row = rng.gen_range(0..dimensions.extended_rows());
		indices.insert(Position { row, col });
	}

	indices.into_iter().collect::<Vec<_>>()
}

#[instrument(skip_all, level = "trace")]
pub async fn get_kate_rows(
	client: &avail::Client,
	rows: Vec<u32>,
	block_hash: H256,
) -> Result<Vec<Option<Vec<u8>>>> {
	let mut params = RpcParams::new();
	params.push(rows)?;
	params.push(block_hash)?;
	let t = client.rpc().deref();
	t.request("kate_queryRows", params)
		.await
		.context("Failed to get Kate rows")
}

/// RPC to get proofs for given positions of block
pub async fn get_kate_proof(
	client: &avail::Client,
	block_hash: H256,
	positions: &[Position],
) -> Result<Vec<Cell>> {
	let mut params = RpcParams::new();
	params.push(positions)?;
	params.push(block_hash)?;
	let t = client.rpc().deref();
	let proofs: Vec<u8> = t
		.request("kate_queryProof", params)
		.await
		.context("Failed to fetch proof")?;

	let i = proofs
		.chunks_exact(CELL_WITH_PROOF_SIZE)
		.map(|chunk| chunk.try_into().expect("chunks of 80 bytes size"));
	Ok(positions
		.iter()
		.zip(i)
		.map(|(&position, &content)| Cell { position, content })
		.collect::<Vec<_>>())
}

// RPC to check connection to substrate node
pub async fn get_system_version(client: &avail::Client) -> Result<String> {
	client
		.rpc()
		.system_version()
		.await
		.context("Failed to retrieve version")
}

pub async fn get_runtime_version(client: &avail::Client) -> Result<RuntimeVersionResult> {
	client
		.rpc()
		.request("state_getRuntimeVersion", RpcParams::new())
		.await
		.context("Failed to retrieve version")
}

/// Shuffles full nodes to randomize access,
/// and pushes last full node to the end of a list
/// so we can try it if connection to other node fails
fn shuffle_full_nodes(full_nodes: &[String], last_full_node: Option<String>) -> Vec<String> {
	let mut candidates = full_nodes.to_owned();
	candidates.retain(|node| Some(node) != last_full_node.as_ref());
	candidates.shuffle(&mut thread_rng());

	// Pushing last full node to the end of a list, if it's only one left to try
	if let (Some(node), true) = (last_full_node, full_nodes.len() != candidates.len()) {
		candidates.push(node);
	}
	candidates
}

pub struct ExpectedVersion<'a> {
	pub version: &'a str,
	pub spec_name: &'a str,
}

impl ExpectedVersion<'_> {
	/// Checks if expected version matches network version.
	/// Since the light client uses subset of the node APIs, `matches` checks only prefix of a node version.
	/// This means that if expected version is `1.6`, versions `1.6.x` of the node will match.
	/// Specification name is checked for exact match.
	/// Since runtime `spec_version` can be changed with runtime upgrade, `spec_version` is removed.
	/// NOTE: Runtime compatiblity check is currently not implemented.
	pub fn matches(&self, node_version: &str, spec_name: &str) -> bool {
		node_version.starts_with(self.version) && self.spec_name == spec_name
	}
}

impl Display for ExpectedVersion<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "v{}/{}", self.version, self.spec_name)
	}
}

#[derive(Clone)]
pub struct Node {
	pub host: String,
	pub system_version: String,
	pub spec_version: u32,
	pub genesis_hash: H256,
}

impl Node {
	pub fn network(&self) -> String {
		format!(
			"{host}/{system_version}/{spec_name}/{spec_version}",
			host = self.host,
			system_version = self.system_version,
			spec_name = EXPECTED_NETWORK_VERSION.spec_name,
			spec_version = self.spec_version,
		)
	}
}

/// Connects to the random full node from the list,
/// trying to connect to the last connected full node as least priority.
pub async fn connect_to_the_full_node(
	full_nodes: &[String],
	last_full_node: Option<String>,
	expected_version: ExpectedVersion<'_>,
) -> Result<(avail::Client, Node)> {
	for full_node_ws in shuffle_full_nodes(full_nodes, last_full_node).iter() {
		let log_warn = |error| {
			warn!("Skipping connection to {full_node_ws}: {error}");
			error
		};

		let Ok(client) = build_client(&full_node_ws, false).await.map_err(log_warn) else {
			continue;
		};
		let Ok(system_version) = get_system_version(&client).await.map_err(log_warn) else {
			continue;
		};
		let Ok(runtime_version) = get_runtime_version(&client).await.map_err(log_warn) else {
			continue;
		};

		let version = format!(
			"v{}/{}/{}",
			system_version, runtime_version.spec_name, runtime_version.spec_version,
		);

		if !expected_version.matches(&system_version, &runtime_version.spec_name) {
			log_warn(anyhow!("expected {expected_version}, found {version}"));
			continue;
		}

		info!("Connection established to the node: {full_node_ws} <{version}>");
		let node = Node {
			host: full_node_ws.clone(),
			system_version,
			spec_version: client.runtime_version().spec_version,
			genesis_hash: client.genesis_hash(),
		};
		return Ok((client, node));
	}
	Err(anyhow!("No working nodes"))
}

/* @note: fn to take the number of cells needs to get equal to or greater than
the percentage of confidence mentioned in config file */

/// Callculates number of cells required to achieve given confidence
pub fn cell_count_for_confidence(confidence: f64) -> u32 {
	let mut cell_count: u32;
	if !(50.0..100f64).contains(&confidence) {
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

#[cfg(test)]
mod tests {
	use crate::rpc::{shuffle_full_nodes, ExpectedVersion};
	use proptest::{
		prelude::any_with,
		prop_assert, prop_assert_eq, proptest,
		sample::size_range,
		strategy::{BoxedStrategy, Strategy},
	};
	use rand::{seq::SliceRandom, thread_rng};
	use test_case::test_case;

	fn full_nodes() -> BoxedStrategy<(Vec<String>, Option<String>)> {
		any_with::<Vec<String>>(size_range(10).lift())
			.prop_map(|nodes| {
				let last_node = nodes.choose(&mut thread_rng()).cloned();
				(nodes, last_node)
			})
			.boxed()
	}

	#[test_case("1.6" , "data_avail" , "1.6.1" , "data_avail" , true; "1.6/data_avail matches 1.6.1/data_avail/0")]
	#[test_case("1.2" , "data_avail" , "1.2.9" , "data_avail" , true; "1.2/data_avail matches 1.2.9/data_avail/0")]
	#[test_case("1.6" , "data_avail" , "1.6.1" , "no_data_avail" , false; "1.6/data_avail matches 1.6.1/no_data_avail/0")]
	#[test_case("1.6" , "data_avail" , "1.7.0" , "data_avail" , false; "1.6/data_avail doesn't match 1.7.0/data_avail/0")]
	#[test_case("1.7" , "data_avail" , "1.7.0" , "data_avail" , true; "1.7/data_avail matches 1.7.0/data_avail/0")]
	fn test_version_match(
		expected_version: &str,
		expected_spec_name: &str,
		version: &str,
		spec_name: &str,
		matches: bool,
	) {
		let expected = ExpectedVersion {
			version: expected_version,
			spec_name: expected_spec_name,
		};

		assert_eq!(expected.matches(version, spec_name), matches);
	}

	proptest! {
		#[test]
		fn shuffle_without_last((full_nodes, _) in full_nodes()) {
			let shuffled = shuffle_full_nodes(&full_nodes, None);
			prop_assert!(shuffled.len() == full_nodes.len());
			prop_assert!(shuffled.iter().all(|node| full_nodes.contains(node)));

			if !full_nodes.contains(&"invalid_node".to_string()) {
				let shuffled = shuffle_full_nodes(&full_nodes, Some("invalid_node".to_string()));
				prop_assert!(shuffled.len() == full_nodes.len());
				prop_assert!(shuffled.iter().all(|node| full_nodes.contains(node)))
			}
		}

		#[test]
		fn shuffle_with_last((full_nodes, last_full_node) in full_nodes()) {
			let last_full_node_count = full_nodes.iter().filter(|&n| Some(n) == last_full_node.as_ref()).count();

			let mut shuffled = shuffle_full_nodes(&full_nodes, last_full_node.clone());
			prop_assert_eq!(shuffled.pop(), last_full_node);

			// Assuming case when last full node occuring more than once in full nodes list
			prop_assert!(shuffled.len() == full_nodes.len() - last_full_node_count);
		}
	}
}
