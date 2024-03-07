use async_trait::async_trait;
use avail_subxt::{primitives::Header, utils::H256};
use codec::Decode;
use color_eyre::{eyre::eyre, Report, Result};
use kate_recovery::matrix::{Dimensions, Position};
use rand::{seq::SliceRandom, thread_rng, Rng};
use serde::{de, Deserialize};
use sp_core::bytes::from_hex;
use std::{
	collections::HashSet,
	fmt::Display,
	sync::{Arc, Mutex},
};
use tokio::{
	sync::broadcast,
	time::{self, timeout},
};
use tracing::{debug, info};

use crate::{
	data::Database,
	network::rpc,
	types::{GrandpaJustification, RetryConfig, State},
};

mod client;
mod subscriptions;

use subscriptions::SubscriptionLoop;
const CELL_SIZE: usize = 32;
const PROOF_SIZE: usize = 48;
pub const CELL_WITH_PROOF_SIZE: usize = CELL_SIZE + PROOF_SIZE;
pub use subscriptions::Event;

pub use client::Client;

pub enum Subscription {
	Header(Header),
	Justification(GrandpaJustification),
}

#[async_trait]
pub trait Command {
	async fn run(&self, client: Client) -> Result<()>;
	fn abort(&self, error: Report);
}

#[derive(Debug, Deserialize, Clone)]
pub struct WrappedJustification(pub GrandpaJustification);

impl Decode for WrappedJustification {
	fn decode<I: codec::Input>(input: &mut I) -> std::result::Result<Self, codec::Error> {
		let j: Vec<u8> = Decode::decode(input)?;
		let jj: GrandpaJustification = Decode::decode(&mut &j[..])?;
		Ok(WrappedJustification(jj))
	}
}

#[derive(Debug, Deserialize, Decode, Clone)]
pub struct FinalityProof {
	/// The hash of block F for which justification is provided.
	pub block: H256,
	/// Justification of the block F.
	pub justification: WrappedJustification,
	/// The set of headers in the range (B; F] that we believe are unknown to the caller. Ordered.
	pub unknown_headers: Vec<Header>,
}

#[derive(Debug, Decode, Clone)]
pub struct WrappedProof(pub FinalityProof);

impl<'de> Deserialize<'de> for WrappedProof {
	fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let data = from_hex(&String::deserialize(deserializer)?)
			.map_err(|e| de::Error::custom(format!("{:?}", e)))?;
		Decode::decode(&mut &data[..]).map_err(|e| de::Error::custom(format!("{:?}", e)))
	}
}

#[derive(Clone, Debug)]
pub struct Node {
	pub host: String,
	pub system_version: String,
	pub spec_name: String,
	pub spec_version: u32,
	pub genesis_hash: H256,
}

impl Node {
	pub fn new(
		host: String,
		system_version: String,
		spec_name: String,
		spec_version: u32,
		genesis_hash: H256,
	) -> Self {
		Self {
			host,
			system_version,
			spec_name,
			spec_version,
			genesis_hash,
		}
	}

	pub fn network(&self) -> String {
		format!(
			"{host}/{system_version}/{spec_name}/{spec_version}",
			host = self.host,
			system_version = self.system_version,
			spec_name = self.spec_name,
			spec_version = self.spec_version,
		)
	}
}

impl Default for Node {
	fn default() -> Self {
		Self {
			host: "{host}".to_string(),
			system_version: "{system_version}".to_string(),
			spec_name: "data-avail".to_string(),
			spec_version: 0,
			genesis_hash: Default::default(),
		}
	}
}

impl Display for Node {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "v{}/{}", self.system_version, self.spec_name)
	}
}

#[derive(Clone)]
pub struct Nodes {
	list: Vec<Node>,
}

impl Nodes {
	pub fn new(nodes: &[String]) -> Self {
		let candidates = nodes.to_owned();
		Self {
			list: candidates
				.iter()
				.map(|s| Node {
					spec_name: Default::default(),
					genesis_hash: Default::default(),
					spec_version: Default::default(),
					system_version: Default::default(),
					host: s.to_string(),
				})
				.collect(),
		}
	}

	/// Shuffles the list of available Nodes, excluding the host used for the current Subxt client creation.
	///
	/// This method returns a new shuffled list of Nodes from the original list, excluding the Node
	/// associated with the current Subxt client host.
	/// The purpose of this exclusion is to prevent accidentally reconnecting to the same host in case of errors.
	/// If there's a need to switch to a different host, the shuffled list provides a randomized order of available Nodes.
	fn shuffle(&self, current_host: String) -> Vec<Node> {
		if self.list.len() <= 1 {
			return self.list.clone();
		}

		let mut list = self
			.list
			.iter()
			.filter(|&Node { host, .. }| host != &current_host)
			.cloned()
			.collect::<Vec<Node>>();
		list.shuffle(&mut thread_rng());
		list
	}

	pub fn iter(&self) -> NodesIterator {
		NodesIterator {
			nodes: self,
			current_index: 0,
		}
	}
}

pub struct NodesIterator<'a> {
	nodes: &'a Nodes,
	current_index: usize,
}

impl<'a> Iterator for NodesIterator<'a> {
	type Item = &'a Node;

	fn next(&mut self) -> Option<Self::Item> {
		let res = self.nodes.list.get(self.current_index);
		self.current_index += 1;
		res
	}
}

pub async fn init<T: Database>(
	db: T,
	state: Arc<Mutex<State>>,
	nodes: &[String],
	genesis_hash: &str,
	retry_config: RetryConfig,
) -> Result<(Client, broadcast::Sender<Event>, SubscriptionLoop<T>)> {
	let rpc_client =
		Client::new(state.clone(), Nodes::new(nodes), genesis_hash, retry_config).await?;
	// create output channel for RPC Subscription Events
	let (event_sender, _) = broadcast::channel(1000);
	let subscriptions =
		SubscriptionLoop::new(state, db, rpc_client.clone(), event_sender.clone()).await?;

	Ok((rpc_client, event_sender, subscriptions))
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

/* @note: fn to take the number of cells needs to get equal to or greater than
the percentage of confidence mentioned in config file */

pub const CELL_COUNT_99_99: u32 = 14;

/// Calculates number of cells required to achieve given confidence
pub fn cell_count_for_confidence(confidence: f64) -> u32 {
	let mut cell_count: u32;
	if !(50.0..=100f64).contains(&confidence) {
		//in this default of 8 cells will be taken
		info!(
			"confidence is {} invalid so taking default confidence of 99",
			confidence
		);
		cell_count = (-((1f64 - (99.3f64 / 100f64)).log2())).ceil() as u32;
	} else {
		cell_count = (-((1f64 - (confidence / 100f64)).log2())).ceil() as u32;
	}
	if cell_count <= 1 {
		info!(
			"confidence of {} is too low so taking confidence of 50.0",
			confidence
		);
		cell_count = 1;
	} else if cell_count > CELL_COUNT_99_99 {
		info!(
			"confidence of {} is invalid so taking confidence of 99.99",
			confidence
		);
		cell_count = CELL_COUNT_99_99;
	}
	cell_count
}

pub async fn wait_for_finalized_header(
	mut rpc_events_receiver: broadcast::Receiver<Event>,
	timeout_seconds: u64,
) -> Result<Header> {
	let timeout_seconds = time::Duration::from_secs(timeout_seconds);
	match timeout(timeout_seconds, rpc_events_receiver.recv()).await {
		Ok(Ok(rpc::Event::HeaderUpdate { header, .. })) => Ok(header),
		Ok(Err(error)) => Err(eyre!("Failed to receive finalized header: {error}")),
		Err(_) => Err(eyre!("Timeout on waiting for first finalized header")),
	}
}
