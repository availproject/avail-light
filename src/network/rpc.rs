use async_trait::async_trait;
use avail_subxt::{avail, build_client, primitives::Header, utils::H256};
use codec::Decode;
use color_eyre::{eyre::eyre, Report, Result};
use futures::{Stream, TryFutureExt, TryStreamExt};
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
use subxt::{rpc::RpcParams, rpc_params};
use tokio::{
	sync::{broadcast, mpsc, RwLock},
	time::{self, timeout},
};
use tokio_retry::Retry;
use tokio_stream::StreamExt;
use tracing::{debug, info, warn};

use crate::{
	consts::ExpectedNodeVariant,
	network::rpc,
	types::{GrandpaJustification, RetryConfig, RuntimeVersion, State, DEV_FLAG_GENHASH},
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
	async fn run(&mut self, client: &avail::Client) -> Result<()>;
	fn abort(&mut self, error: Report);
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

#[derive(Clone, Debug)]
pub struct Node {
	pub host: String,
	pub system_version: String,
	pub spec_name: String,
	pub spec_version: u32,
	pub genesis_hash: String,
}

impl Node {
	pub fn new(
		host: String,
		system_version: String,
		spec_name: String,
		spec_version: u32,
		genesis_hash: &str,
	) -> Self {
		Self {
			host,
			system_version,
			spec_name,
			spec_version,
			genesis_hash: genesis_hash.to_string(),
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
		let mut list: Vec<Node> = self
			.list
			.iter()
			.cloned()
			.filter(|Node { host, .. }| host != &current_host)
			.collect();
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

enum Subscription {
	Header(Header),
	Justification(GrandpaJustification),
}

#[derive(Clone)]
struct RpcClient {
	subxt_client: Arc<RwLock<avail::Client>>,
	nodes: Nodes,
	state: Arc<Mutex<State>>,
	genesis_hash: String,
	retry_config: RetryConfig,
}

impl RpcClient {
	pub async fn new(
		state: Arc<Mutex<State>>,
		nodes: Nodes,
		genesis_hash: &str,
		retry_config: RetryConfig,
	) -> Result<Self> {
		// try and connect appropriate Node from the provided list
		// will do retries with the provided Retry Config
		let (client, node, _) = Retry::spawn(retry_config.iter(), || async {
			Self::connect_nodes_until_success(
				nodes.shuffle(Default::default()),
				ExpectedNodeVariant::new(),
				genesis_hash,
				|_| futures::future::ok(()),
			)
			.await
		})
		.await?;

		// update global state with the currently connected Node
		state.lock().unwrap().connected_node = node;

		Ok(Self {
			subxt_client: Arc::new(RwLock::new(client)),
			state,
			genesis_hash: genesis_hash.to_string(),
			nodes,
			retry_config,
		})
	}

	async fn connect_nodes_until_success<T, Fun, Fut>(
		nodes: Vec<Node>,
		expected_node: ExpectedNodeVariant,
		expected_genesis_hash: &str,
		mut predicate: Fun,
	) -> Result<(avail::Client, Node, T)>
	where
		Fun: FnMut(avail::Client) -> Fut,
		Fut: std::future::Future<Output = Result<T>>,
	{
		// go through the provided list of Nodes to try and find and appropriate one,
		// after a successful connection, try to execute passed call predicate
		for Node { host, .. } in nodes.iter() {
			let result = Self::create_subxt_client(
				&host,
				expected_node.clone(),
				expected_genesis_hash.clone(),
			)
			.and_then(|(client, node)| predicate(client.clone()).map_ok(|res| (client, node, res)))
			.await;

			match result {
				Err(error) => warn!(host, %error, "Skipping connection with this node"),
				ok => return ok,
			}
		}

		Err(eyre!("Failed to connect any appropriate working node"))
	}

	async fn create_subxt_client(
		host: &str,
		expected_node: ExpectedNodeVariant,
		expected_genesis_hash: &str,
	) -> Result<(avail::Client, Node)> {
		let (client, _) = build_client(host, false).await.map_err(|e| eyre!(e))?;

		// check genesis hash
		let genesis_hash = client.genesis_hash();
		info!("Genesis hash: {:?}", genesis_hash);
		if let Some(cfg_genhash) = from_hex(expected_genesis_hash)
			.ok()
			.and_then(|e| TryInto::<[u8; 32]>::try_into(e).ok().map(H256::from))
		{
			if !genesis_hash.eq(&cfg_genhash) {
				Err(eyre!(
					"Genesis hash doesn't match the configured one! Change the config or the node url ({}).", host
				))?
			}
		} else if expected_genesis_hash.starts_with(DEV_FLAG_GENHASH) {
			warn!("Genesis hash configured for development ({}), skipping the genesis hash check entirely.", expected_genesis_hash);
		} else {
			Err(eyre!(
				"Genesis hash invalid, badly configured or missing (\"{}\").",
				expected_genesis_hash
			))?
		};

		// check system and runtime versions
		let system_version = client.rpc().system_version().await?;
		let runtime_version: RuntimeVersion = client
			.rpc()
			.request("state_getRuntimeVersion", RpcParams::new())
			.await?;

		if !expected_node.matches(&system_version, &runtime_version.spec_name) {
			return Err(eyre!(
				"Expected Node system version:{}, found: {}. Skipping to another node.",
				expected_node.system_version,
				system_version.clone()
			));
		}

		let variant = Node::new(
			host.to_string(),
			system_version,
			runtime_version.spec_name,
			runtime_version.spec_version,
			&genesis_hash.to_string(),
		);

		Ok((client, variant))
	}

	async fn create_subxt_subscriptions(
		client: avail::Client,
	) -> Result<impl Stream<Item = Result<Subscription, subxt::error::Error>>> {
		// create Header subscription
		let header_subscription = client.rpc().subscribe_finalized_block_headers().await?;
		// map Header subscription to the same type for later matching
		let headers = header_subscription.map_ok(|h| Subscription::Header(h));

		let justification_subscription = client
			.rpc()
			.subscribe(
				"grandpa_subscribeJustifications",
				rpc_params![],
				"grandpa_unsubscribeJustifications",
			)
			.await?;
		// map Justification subscription to the same type for later matching
		let justifications = justification_subscription.map_ok(|j| Subscription::Justification(j));

		Ok(headers.merge(justifications))
	}

	async fn with_retries<F, Fut, T>(&self, mut predicate: F) -> Result<T>
	where
		F: FnMut(avail::Client) -> Fut,
		Fut: std::future::Future<Output = Result<T>>,
	{
		// try and execute the passed function, use the Retry strategy if needed
		let mut test = move || async move {
			let client = self.current_client().await;
			let res = predicate(client).await?;
			Ok::<T, Report>(res)
		};

		if let Ok(result) = Retry::spawn(self.retry_config.iter(), test).await {
			// this was successful, return early
			return Ok(result);
		}

		// if not, find another Node where this could still be done
		let mut state = self.state.lock().unwrap();
		let nodes = self.nodes.shuffle(state.connected_node.host.clone());
		let (client, node, result) = Self::connect_nodes_until_success(
			nodes,
			ExpectedNodeVariant::new(),
			&state.connected_node.genesis_hash,
			move |client| predicate(client).map_err(Report::from),
		)
		.await?;

		// retries gave results, update currently connected Node and created Client
		state.connected_node = node;
		*self.subxt_client.write().await = client;

		Ok(result)
	}

	async fn subscription_stream(self) -> impl Stream<Item = Result<Subscription>> {
		async_stream::stream! {
			'outer: loop{
				let mut stream = match self.with_retries(|client| async move{
					Self::create_subxt_subscriptions(client).await
				}).await {
					Ok(s) => s,
					Err(err) => {
						yield Err(err);
						return;
					}
				};

				loop {
					// no more subscriptions left on stream, we have to return
					let Some(result) = stream.next().await else { return};
					// if Error was received, we need to switch to another RPC Client
					let Ok(item) = result else { continue 'outer};
					yield Ok(item);
				}
			}
		}
	}

	async fn current_client(&self) -> avail::Client {
		self.subxt_client.read().await.clone()
	}
}

pub async fn init(
	db: Arc<DB>,
	state: Arc<Mutex<State>>,
	nodes: &[String],
	genesis_hash: &str,
	retry_config: RetryConfig,
) -> Result<(Client, broadcast::Sender<Event>, EventLoop)> {
	// create channel for Event Loop Commands
	let (command_sender, command_receiver) = mpsc::channel(1000);
	// create output channel for RPC Subscription Events
	let (event_sender, _) = broadcast::channel(1000);

	let rpc_client =
		RpcClient::new(state.clone(), Nodes::new(nodes), genesis_hash, retry_config).await?;

	Ok((
		Client::new(command_sender),
		event_sender.clone(),
		EventLoop::new(rpc_client, state, db, command_receiver, event_sender).await?,
	))
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
