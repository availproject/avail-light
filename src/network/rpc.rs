use anyhow::anyhow;
use async_trait::async_trait;
use avail_subxt::{avail, primitives::Header, utils::H256};
use codec::Decode;
use kate_recovery::matrix::{Dimensions, Position};
use rand::{seq::SliceRandom, thread_rng, Rng};
use rocksdb::DB;
use serde::{de, Deserialize};
use sp_core::bytes::from_hex;
use std::{
	collections::HashSet,
	fmt::Display,
	sync::{Arc, Mutex},
};
use tokio::{
	sync::{broadcast, mpsc},
	time::{self, timeout},
};
use tracing::debug;

use crate::{
	consts::EXPECTED_NETWORK_VERSION,
	network::rpc,
	types::{GrandpaJustification, State},
};

mod client;
mod event_loop;

pub use client::Client;
use event_loop::EventLoop;
const CELL_SIZE: usize = 32;
const PROOF_SIZE: usize = 48;
pub const CELL_WITH_PROOF_SIZE: usize = CELL_SIZE + PROOF_SIZE;
pub use event_loop::Event;

#[async_trait]
pub trait Command {
	async fn run(&mut self, client: &avail::Client) -> anyhow::Result<(), anyhow::Error>;
	fn abort(&mut self, error: anyhow::Error);
}

type SendableCommand = Box<dyn Command + Send + Sync>;
type CommandSender = mpsc::Sender<SendableCommand>;
type CommandReceiver = mpsc::Receiver<SendableCommand>;

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

	pub fn with_spec_version(&mut self, spec_version: u32) -> &mut Self {
		self.spec_version = spec_version;
		self
	}

	pub fn with_system_version(&mut self, system_version: String) -> &mut Self {
		self.system_version = system_version;
		self
	}

	pub fn with_genesis_hash(&mut self, genesis_hash: H256) -> &mut Self {
		self.genesis_hash = genesis_hash;
		self
	}
}

impl Default for Node {
	fn default() -> Self {
		Self {
			host: "{host}".to_string(),
			system_version: "{system_version}".to_string(),
			spec_version: 0,
			genesis_hash: H256::default(),
		}
	}
}

pub struct Nodes {
	list: Vec<Node>,
	current_index: usize,
}

impl Nodes {
	pub fn next_node(&mut self) -> Option<Node> {
		// we have exhausted all nodes from the list
		// this is the last one
		if self.current_index == self.list.len() - 1 {
			None
		} else {
			// increment current index
			self.current_index += 1;
			self.get_current()
		}
	}

	pub fn get_current(&self) -> Option<Node> {
		let node = &self.list[self.current_index];
		Some(node.clone())
	}

	pub fn new(nodes: &[String]) -> Self {
		let candidates = nodes.to_owned();
		Self {
			list: candidates
				.iter()
				.map(|s| Node {
					genesis_hash: Default::default(),
					spec_version: Default::default(),
					system_version: Default::default(),
					host: s.to_string(),
				})
				.collect(),
			current_index: 0,
		}
	}

	fn shuffle(&mut self) {
		self.list.shuffle(&mut thread_rng());
	}

	fn reset(&mut self) -> Option<Node> {
		// shuffle the available list of nodes
		self.shuffle();
		// set the current index to the first one
		self.current_index = 0;
		self.get_current()
	}
}

#[derive(Debug)]
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
	/// NOTE: Runtime compatibility check is currently not implemented.
	pub fn matches(&self, node_version: &str, spec_name: &str) -> bool {
		node_version.starts_with(self.version) && self.spec_name == spec_name
	}
}

impl Display for ExpectedVersion<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "v{}/{}", self.version, self.spec_name)
	}
}

pub fn init(
	db: Arc<DB>,
	state: Arc<Mutex<State>>,
	nodes: &[String],
) -> (Client, broadcast::Sender<Event>, EventLoop) {
	// create channel for Event Loop Commands
	let (command_sender, command_receiver) = mpsc::channel(1000);
	// create output channel for RPC Subscription Events
	let (event_sender, _) = broadcast::channel(1000);

	(
		Client::new(command_sender),
		event_sender.clone(),
		EventLoop::new(db, state, Nodes::new(nodes), command_receiver, event_sender),
	)
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

/// Calculates number of cells required to achieve given confidence
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

pub async fn wait_for_finalized_header(
	mut rpc_events_receiver: broadcast::Receiver<Event>,
	timeout_seconds: u64,
) -> anyhow::Result<Header> {
	let timeout_seconds = time::Duration::from_secs(timeout_seconds);
	match timeout(timeout_seconds, rpc_events_receiver.recv()).await {
		Ok(Ok(rpc::Event::HeaderUpdate { header, .. })) => Ok(header),
		Ok(Err(error)) => Err(anyhow!("Failed to receive finalized header: {error}")),
		Err(_) => Err(anyhow!("Timeout on waiting for first finalized header")),
	}
}
