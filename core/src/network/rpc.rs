use avail_rust::{
	kate_recovery::matrix::{Dimensions, Position},
	sp_core::bytes::from_hex,
	AvailHeader, H256,
};
use codec::{Decode, Encode};
use color_eyre::{eyre::eyre, Result};
use configuration::RPCConfig;
use rand::{seq::SliceRandom, thread_rng, Rng};
use serde::{de, Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
use std::{collections::HashSet, fmt::Display};
use tokio::{
	sync::broadcast::{Receiver, Sender},
	time::{timeout, Duration},
};
use tracing::{debug, info};
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

mod client;
pub mod configuration;
mod subscriptions;

use crate::{
	data::Database, network::rpc::OutputEvent as RpcEvent, shutdown::Controller,
	types::GrandpaJustification,
};
pub use client::Client;
use subscriptions::SubscriptionLoop;

const CELL_SIZE: usize = 32;
const PROOF_SIZE: usize = 48;
pub const CELL_WITH_PROOF_SIZE: usize = CELL_SIZE + PROOF_SIZE;

pub enum Subscription {
	Header(AvailHeader),
	Justification(GrandpaJustification),
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum OutputEvent {
	InitialConnection(String),
	HeaderUpdate {
		header: AvailHeader,
		received_at: Instant,
		received_at_timestamp: u64,
	},
	SwitchedConnection(String),
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
	pub unknown_headers: Vec<AvailHeader>,
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

#[derive(Clone, Debug, Serialize, Deserialize, Decode, Encode)]
pub struct Node {
	pub host: String,
	pub system_version: String,
	pub spec_version: u32,
	pub genesis_hash: H256,
}

impl Node {
	pub fn new(
		host: String,
		system_version: String,
		spec_version: u32,
		genesis_hash: H256,
	) -> Self {
		Self {
			host,
			system_version,
			spec_version,
			genesis_hash,
		}
	}

	pub fn network(&self) -> String {
		format!(
			"{host}/{system_version}/{spec_version}",
			host = self.host,
			system_version = self.system_version,
			spec_version = self.spec_version,
		)
	}
}

impl Default for Node {
	fn default() -> Self {
		Self {
			host: "{host}".to_string(),
			system_version: "{system_version}".to_string(),
			spec_version: 0,
			genesis_hash: Default::default(),
		}
	}
}

impl Display for Node {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "v{}", self.system_version)
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
					genesis_hash: Default::default(),
					spec_version: Default::default(),
					system_version: Default::default(),
					host: s.to_string(),
				})
				.collect(),
		}
	}

	/// Shuffles the list of available Nodes, partitioning the host used for the current Subxt client creation.
	///
	/// This method returns a new shuffled list of Nodes from the original list, and the Node
	/// associated with the current Subxt client host.
	/// The purpose of this partitioning is to prevent accidentally reconnecting to the same host in case of errors.
	/// If there's a need to switch to a different host, the shuffled list provides a randomized order of available Nodes.
	fn shuffle(&self, current_host: &str) -> (Vec<Node>, Vec<Node>) {
		let nodes = self.list.clone();

		if nodes.len() <= 1 {
			return (nodes, vec![]);
		}

		let (mut first, second): (Vec<_>, Vec<_>) = nodes
			.into_iter()
			.partition(|Node { host, .. }| host != current_host);
		first.shuffle(&mut thread_rng());
		(first, second)
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

pub async fn init<T: Database + Clone>(
	db: T,
	genesis_hash: &str,
	rpc: &RPCConfig,
	shutdown: Controller<String>,
	event_sender: Sender<RpcEvent>,
) -> Result<(Client<T>, SubscriptionLoop<T>)> {
	let rpc_client = Client::new(
		db.clone(),
		Nodes::new(&rpc.full_node_ws),
		genesis_hash,
		rpc.retry.clone(),
		shutdown,
		event_sender.clone(),
	)
	.await?;
	let subscriptions = SubscriptionLoop::new(db, rpc_client.clone(), event_sender.clone()).await?;

	Ok((rpc_client, subscriptions))
}

/// Generates random cell positions for sampling
pub fn generate_random_cells(dimensions: Dimensions, cell_count: u32) -> Vec<Position> {
	let (max_cells, row_limit) = {
		#[cfg(feature = "multiproof")]
		{
			(dimensions.size(), dimensions.rows().get().into())
		}
		#[cfg(not(feature = "multiproof"))]
		{
			(dimensions.extended_size(), dimensions.extended_rows())
		}
	};

	let count = if max_cells < cell_count {
		debug!("Max cells count {max_cells} is lesser than cell_count {cell_count}");
		max_cells
	} else {
		cell_count
	};

	let mut rng = thread_rng();
	let mut indices = HashSet::new();
	while (indices.len() as u32) < count {
		let col = rng.gen_range(0..dimensions.cols().into());
		let row = rng.gen_range(0..row_limit);
		indices.insert(Position { row, col });
	}

	indices.into_iter().collect()
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
	mut rpc_events_receiver: Receiver<OutputEvent>,
	timeout_seconds: u64,
) -> Result<AvailHeader> {
	let timeout_seconds = Duration::from_secs(timeout_seconds);

	let result = timeout(timeout_seconds, async {
		while let Ok(event) = rpc_events_receiver.recv().await {
			if let OutputEvent::HeaderUpdate { header, .. } = event {
				return Ok(header);
			}
			// silently skip ConnectedHost event
		}
		Err(eyre!("RPC event receiver chanel closed"))
	})
	.await;

	match result {
		Ok(header) => header,
		Err(_) => Err(eyre!("Timeout while waiting for first finalized header")),
	}
}
