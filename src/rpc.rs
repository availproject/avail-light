//! RPC communication with avail node.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use avail_subxt::utils::AccountId32;
use avail_subxt::{
	avail, build_client,
	primitives::Header as DaHeader,
	rpc::{types::BlockNumber, RpcParams},
	utils::H256,
};
use futures::prelude::*;
use itertools::Either;
use kate_recovery::{
	data::Cell,
	matrix::{Dimensions, Position},
};
use rand::{seq::SliceRandom, thread_rng, Rng};
use rocksdb::DB;
use sp_core::ed25519;
use std::{collections::HashSet, fmt::Display};
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

use crate::sync_finality::WrappedProof;
use crate::{consts::EXPECTED_NETWORK_VERSION, types::*};

#[derive(Debug, Clone)]
pub struct RpcClient {
	client: Arc<RwLock<(avail::Client, Node)>>,
	nodes: Vec<String>,
	expected_version: ExpectedVersion<'static>,
	db: Option<Arc<DB>>,
	backoff: backoff::ExponentialBackoff,
}

impl RpcClient {
	async fn connect(
		full_node_ws: &str,
		expected_version: &ExpectedVersion<'_>,
		expected_genesis_hash: Option<H256>,
	) -> Result<(avail::Client, Node)> {
		tracing::debug!(full_node_ws, "Trying to connect to rpc");
		let client = build_client(&full_node_ws, false).await?;
		let system_version: String = client
			.rpc()
			.system_version()
			.await
			.context("Failed to retrieve system version")?;
		let runtime_version: RuntimeVersionResult = client
			.rpc()
			.request("state_getRuntimeVersion", RpcParams::new())
			.await
			.context("Failed to retrieve runtime version")?;

		let version = format!(
			"v{}/{}/{}",
			system_version, runtime_version.spec_name, runtime_version.spec_version,
		);

		if !expected_version.matches(&system_version, &runtime_version.spec_name) {
			return Err(anyhow!(
				"expected version {expected_version}, found {version}"
			));
		}
		let genesis_hash = client.genesis_hash();
		match expected_genesis_hash {
			Some(hash) if hash != genesis_hash => {
				return Err(anyhow!(
					"expected genesis hash {hash}, found {genesis_hash}"
				));
			},
			_ => (),
		}

		info!("Connection established to the node: {full_node_ws} <{version}>");
		let node = Node {
			host: full_node_ws.to_owned(),
			system_version,
			spec_version: client.runtime_version().spec_version,
			genesis_hash,
		};

		Ok((client, node))
	}

	async fn connect_to_available_rpc_and_return_call<T, F, Fut>(
		full_nodes: &[String],
		expected_version: &ExpectedVersion<'static>,
		expected_genesis_hash: Option<H256>,
		mut f: F,
	) -> Result<(avail::Client, Node, T)>
	where
		F: FnMut(avail::Client) -> Fut + Copy,
		Fut: std::future::Future<Output = Result<T>>,
	{
		full_nodes
			.iter()
			.map(|address| {
				Self::connect(address, expected_version, expected_genesis_hash)
					.and_then(move |(client, node)| {
						f(client.clone()).map_ok(|ret| (client, node, ret))
					})
					.inspect_err(move |error| warn!(address, %error, "Skipping connection"))
			})
			.collect::<futures::stream::FuturesUnordered<_>>()
			.skip_while(|res| futures::future::ready(res.is_err()))
			.map(Result::unwrap)
			.next()
			.await
			.context("Failed to connect to a working node")
	}

	/// Shuffles full nodes to randomize access,
	/// and pushes last full node to the end of a list
	/// so we can try it if connection to other node fails
	fn shuffle_full_nodes(full_nodes: &mut Vec<String>, last_full_node: Option<&String>) {
		let old_len = full_nodes.len();
		full_nodes.retain(|node| Some(node) != last_full_node);
		full_nodes.shuffle(&mut thread_rng());

		// Pushing last full node to the end of a list, if it's only one left to try
		if let (Some(node), true) = (last_full_node, old_len != full_nodes.len()) {
			full_nodes.push(node.clone());
		}
	}

	pub async fn new(
		nodes: Vec<String>,
		expected_version: ExpectedVersion<'static>,
		db: Option<Arc<DB>>,
		backoff: backoff::ExponentialBackoff,
	) -> Result<Self> {
		let expected_genesis_hash = if let Some(db) = &db {
			crate::data::get_genesis_hash(db.clone())?
		} else {
			None
		};
		let (client, node, ()) = Self::connect_to_available_rpc_and_return_call(
			&nodes,
			&expected_version,
			expected_genesis_hash,
			|_| futures::future::ok(()),
		)
		.await?;

		info!(?node.genesis_hash);
		if let Some(db) = &db {
			crate::data::store_last_full_node_ws_in_db(db.clone(), node.host.clone())?;
			if expected_genesis_hash.is_none() {
				info!("No genesis hash is found in the db, storing the new hash now.");
				crate::data::store_genesis_hash(db.clone(), node.genesis_hash)?;
			}
		}

		Ok(Self {
			client: Arc::new(RwLock::new((client, node))),
			nodes,
			expected_version,
			db,
			backoff,
		})
	}

	async fn current_client(&self) -> avail::Client {
		self.client.read().await.0.clone()
	}

	pub async fn current_node(&self) -> Node {
		self.client.read().await.1.clone()
	}

	async fn with_client<F, Fut, T>(&self, mut f: F) -> Result<T>
	where
		F: FnMut(avail::Client) -> Fut + Copy,
		Fut: std::future::Future<Output = Result<T, subxt::error::Error>>,
	{
		let either = backoff::future::retry(self.backoff.clone(), move || async move {
			// Try with current client
			match f(self.current_client().await).await {
				Ok(ok) => return Ok(Either::Left(ok)),
				Err(error) => {
					warn!(%error, "Failed to connect to node. Trying to reach to another one");
				},
			}
			let last_node = self.current_node().await;

			let mut nodes = self.nodes.clone();
			// Shuffle nodes discarding the last one
			Self::shuffle_full_nodes(&mut nodes, Some(&last_node.host));
			// Trying to find some node which is available
			Self::connect_to_available_rpc_and_return_call(
				&nodes,
				&self.expected_version,
				Some(last_node.genesis_hash),
				move |cl| f(cl).map_err(anyhow::Error::from),
			)
			.await
			.map_err(backoff::Error::transient)
			.map(Either::Right)
		})
		.await
		.context("Failed to reach to any node")?;

		let (client, node, ok) = match either {
			// If current client succeeded we don't need to update DB and structure
			Either::Left(ok) => return Ok(ok),
			Either::Right(ret) => ret,
		};

		// Update last full node in DB
		if let Some(db) = &self.db {
			crate::data::store_last_full_node_ws_in_db(db.clone(), node.host.clone())?;
		}

		// And in structure
		*self.client.write().await = (client, node);

		Ok(ok)
	}

	fn with_client_subscribe<T, F, Fut>(self, f: F) -> impl Stream<Item = anyhow::Result<T>>
	where
		F: FnMut(avail::Client) -> Fut + Copy,
		Fut: std::future::Future<
			Output = Result<avail_subxt::rpc::Subscription<T>, subxt::error::Error>,
		>,
		T: serde::de::DeserializeOwned,
	{
		async_stream::stream! {
			'outer: loop  {
				let mut stream = match self.with_client(f).await {
					Ok(s) => s,
					Err(err) => {
						yield Err(err);
						return;
					}
				};

				loop {
					// We need to return, as stream has ended
					let Some(result) = stream.next().await else { return };
					// If we received error that means that we need to find a new client
					let Ok(res) = result else { continue 'outer };
					yield Ok(res);
				}
			}
		}
	}

	pub fn subscribe_finalized_block_headers(
		self,
	) -> impl Stream<Item = anyhow::Result<avail_subxt::primitives::Header>> {
		self.with_client_subscribe(|client| async move {
			client.rpc().subscribe_finalized_block_headers().await
		})
	}

	pub fn subscribe_grandpa_justifications(
		self,
	) -> impl Stream<Item = anyhow::Result<GrandpaJustification>> {
		self.with_client_subscribe(|client| async move {
			client
				.rpc()
				.subscribe(
					"grandpa_subscribeJustifications",
					avail_subxt::rpc::rpc_params![],
					"grandpa_unsubscribeJustifications",
				)
				.await
		})
	}

	pub async fn get_block_hash(&self, block: u32) -> Result<H256> {
		self.with_client(move |client| async move {
			client
				.rpc()
				.block_hash(Some(BlockNumber::from(block)))
				.await
		})
		.await?
		.ok_or_else(|| anyhow!("Block with number {block} not found"))
	}

	pub async fn get_header_by_hash(&self, hash: H256) -> Result<DaHeader> {
		self.with_client(move |client| async move { client.rpc().header(Some(hash)).await })
			.await?
			.ok_or_else(|| anyhow!("Header with hash {hash:?} not found"))
	}

	pub async fn get_valset_by_hash(&self, hash: H256) -> Result<Vec<ed25519::Public>> {
		let grandpa_valset: Vec<(ed25519::Public, u64)> = self
			.with_client(move |client| async move {
				client
					.runtime_api()
					.at(hash)
					.call_raw("GrandpaApi_grandpa_authorities", None)
					.await
			})
			.await?;

		// Drop weights, as they are not currently used.
		Ok(grandpa_valset.iter().map(|e| e.0).collect())
	}

	pub async fn get_valset_by_block_number(&self, block: u32) -> Result<Vec<ed25519::Public>> {
		let hash = self.get_block_hash(block).await?;
		self.get_valset_by_hash(hash).await
	}

	pub async fn get_session_valset_by_hash(&self, hash: H256) -> Result<Option<Vec<AccountId32>>> {
		self.with_client(move |client| async move {
			let validators_key = avail_subxt::api::storage().session().validators();
			client.storage().at(hash).fetch(&validators_key).await
		})
		.await
		.context("Couldn't get initial validator set")
	}

	fn storage_key(keys: impl IntoIterator<Item = impl AsRef<[u8]>>) -> Vec<u8> {
		let mut storage_key = Vec::new();
		for k in keys {
			storage_key.extend_from_slice(&sp_core::twox_128(k.as_ref()));
		}
		storage_key
	}

	pub async fn all_grandpa_session_keys_since(
		&self,
		hash: H256,
	) -> Result<impl Iterator<Item = ed25519::Public>> {
		const GRANDPA_KEY_ID: [u8; 4] = *b"gran";
		const GRANDPA_KEY_LEN: usize = 32;

		let key = Self::storage_key(["Session".as_bytes(), b"KeyOwner"]);

		let keys = self
			.with_client(|client| {
				let key = &key;
				async move {
					// TODO: Check paging
					client
						.rpc()
						// Get all storage keys that correspond to Session_KeyOwner query, then filter by "gran"
						// Perhaps there is a better way, but I don't know it
						.storage_keys_paged(key, 1000, None, Some(hash))
						.await
				}
			})
			.await
			.context("Couldn't get storage keys associated with key owners!")?
			.into_iter()
			// throw away the beginning, we don't need it
			.map(move |e| e.0[key.len()..].to_vec())
			.filter(|e| {
				// exclude the actual key (at the end of the storage key) from search, we may find "gran" by accident
				e[..(e.len() - GRANDPA_KEY_LEN)]
					.windows(GRANDPA_KEY_ID.len())
					.any(|e| e == GRANDPA_KEY_ID)
			})
			.map(|e| e.as_slice()[e.len() - GRANDPA_KEY_LEN..].to_vec())
			.map(|e| ed25519::Public::from_raw(e.try_into().expect("Vector isn't 32 bytes long")));

		Ok(keys)
	}

	pub async fn get_session_key_owner(
		&self,
		key_type: avail_subxt::api::runtime_types::sp_core::crypto::KeyTypeId,
		key: ed25519::Public,
		at: H256,
	) -> Result<Option<AccountId32>> {
		self.with_client(|client| {
			let key_type = key_type.clone();
			async move {
				let session_key_key_owner = avail_subxt::api::storage()
					.session()
					.key_owner(key_type, key.0);
				client.storage().at(at).fetch(&session_key_key_owner).await
			}
		})
		.await
		.context("Couldn't get session key owner for grandpa key!")
	}

	pub async fn get_grandpa_finality_proof(&self, block_n: u32) -> Result<WrappedProof> {
		self.with_client(|client| async move {
			client
				.rpc()
				.request(
					"grandpa_proveFinality",
					avail_subxt::rpc::rpc_params![block_n],
				)
				.await
		})
		.await
		.with_context(|| format!("Couldn't get finality proof for block no. {block_n}",))
	}

	pub async fn get_grandpa_set_id(&self, at: H256) -> Result<Option<u64>> {
		self.with_client(|client| async move {
			let set_id_key = avail_subxt::api::storage().grandpa().current_set_id();
			client.storage().at(at).fetch(&set_id_key).await
		})
		.await
		.with_context(|| format!("Couldn't get set_id at {at}"))
	}

	/// RPC for obtaining header of latest finalized block mined by network
	pub async fn get_chain_head_header(&self) -> Result<DaHeader> {
		let h = self
			.with_client(|client| async move { client.rpc().finalized_head().await })
			.await?;
		self.with_client(move |client| async move { client.rpc().header(Some(h)).await })
			.await?
			.ok_or_else(|| anyhow!("Couldn't get latest finalized header"))
	}

	pub async fn get_chain_head_hash(&self) -> Result<H256> {
		self.with_client(|client| async move { client.rpc().finalized_head().await })
			.await
			.context("Cannot get finalized head hash")
	}

	pub async fn get_set_id_by_hash(&self, hash: H256) -> Result<u64> {
		self.with_client(move |client| {
			let set_id_key = avail_subxt::api::storage().grandpa().current_set_id();
			async move {
				client
					// Fetch the set ID from storage at current height
					.storage()
					// None means current height
					.at(hash)
					.fetch(&set_id_key)
					.await
			}
		})
		.await
		.map(|opt| opt.expect("The set_id should exist"))
	}

	pub async fn get_set_id_by_block_number(&self, block: u32) -> Result<u64> {
		let hash = self.get_block_hash(block).await?;
		self.get_set_id_by_hash(hash).await
	}

	/// Gets header by block number
	pub async fn get_header_by_block_number(&self, block: u32) -> Result<(DaHeader, H256)> {
		let hash = self.get_block_hash(block).await?;
		self.get_header_by_hash(hash).await.map(|e| (e, hash))
	}

	#[instrument(skip_all, level = "trace")]
	pub async fn get_kate_rows(
		&self,
		rows: Vec<u32>,
		block_hash: H256,
	) -> Result<Vec<Option<Vec<u8>>>> {
		let mut params = RpcParams::new();
		params.push(rows)?;
		params.push(block_hash)?;
		self.with_client(|client| {
			let params = params.clone();
			async move { client.rpc().request("kate_queryRows", params).await }
		})
		.await
	}

	/// RPC to get proofs for given positions of block
	pub async fn get_kate_proof(
		&self,
		block_hash: H256,
		positions: &[Position],
	) -> Result<Vec<Cell>> {
		let mut params = RpcParams::new();
		params.push(positions)?;
		params.push(block_hash)?;

		let proofs: Vec<u8> = self
			.with_client(|client| {
				let params = &params;
				async move {
					client
						.rpc()
						.request("kate_queryProof", params.clone())
						.await
				}
			})
			.await
			.context("Error fetching proof")?;

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
	pub async fn get_system_version(&self) -> Result<String> {
		self.with_client(|client| async move { client.rpc().system_version().await })
			.await
			.context("Version couldn't be retrieved")
	}

	pub async fn get_runtime_version(&self) -> Result<RuntimeVersionResult> {
		self.with_client(|client| async move {
			client
				.rpc()
				.request("state_getRuntimeVersion", RpcParams::new())
				.await
		})
		.await
		.context("Version couldn't be retrieved, error")
	}
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

#[derive(Debug, Clone, Copy)]
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

#[derive(Debug, Clone)]
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
	use crate::rpc::{ExpectedVersion, RpcClient};
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
			let mut shuffled = full_nodes.clone();
			RpcClient::shuffle_full_nodes(&mut shuffled, None);
			prop_assert!(shuffled.len() == full_nodes.len());
			prop_assert!(shuffled.iter().all(|node| full_nodes.contains(node)));

			if !full_nodes.contains(&"invalid_node".to_string()) {
				let mut shuffled = full_nodes.clone();
				RpcClient::shuffle_full_nodes(&mut shuffled, Some(&"invalid_node".to_string()));
				prop_assert!(shuffled.len() == full_nodes.len());
				prop_assert!(shuffled.iter().all(|node| full_nodes.contains(node)))
			}
		}

		#[test]
		fn shuffle_with_last((full_nodes, last_full_node) in full_nodes()) {
			let last_full_node_count = full_nodes.iter().filter(|&n| Some(n) == last_full_node.as_ref()).count();

			let mut shuffled = full_nodes.clone();
			RpcClient::shuffle_full_nodes(&mut shuffled, last_full_node.as_ref());
			prop_assert_eq!(shuffled.pop(), last_full_node);

			// Assuming case when last full node occuring more than once in full nodes list
			prop_assert!(shuffled.len() == full_nodes.len() - last_full_node_count);
		}
	}
}
