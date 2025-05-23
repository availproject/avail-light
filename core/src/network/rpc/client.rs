use avail_rust::{
	avail::{self, runtime_types::sp_core::crypto::KeyTypeId},
	avail_core::AppId,
	kate_recovery::{data::{Cell, SingleCell}, matrix::Position},
	primitives::kate::{Cells, GProof, GRawScalar, Rows},
	rpc::{
		chain::{get_block_hash, get_finalized_head},
		kate::{query_proof, query_rows},
		state::get_runtime_version,
		system::version,
	},
	sp_core::{bytes::from_hex, crypto, ed25519::Public},
	subxt::{
		self,
		backend::{
			legacy::rpc_methods::{RuntimeVersion, StorageKey},
			rpc::RpcClient as SubxtRpcClient,
		},
		rpc_params,
		tx::SubmittableExtrinsic,
		utils::AccountId32,
	},
	AOnlineClient, AvailHeader, Client as AvailRpcClient, Keypair, Options, H256, SDK, U256,
};
use color_eyre::{
	eyre::{eyre, WrapErr},
	Report, Result,
};
use futures::{Stream, TryStreamExt};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
use std::{iter::Iterator, pin::Pin, sync::Arc};
#[cfg(not(target_arch = "wasm32"))]
use thiserror::Error;
#[cfg(target_arch = "wasm32")]
use thiserror_no_std::Error;
use tokio::sync::{broadcast::Sender, RwLock};
use tokio_retry::Retry;
#[cfg(not(target_arch = "wasm32"))]
use tokio_stream::Elapsed;
use tokio_stream::{StreamExt, StreamMap};
use tracing::{error, info, warn};

use super::{configuration::RetryConfig, Node, Nodes, Subscription, WrappedProof};
use crate::{
	data::{Database, RpcNodeKey, SignerNonceKey},
	network::rpc::OutputEvent as RpcEvent,
	shutdown::Controller,
	types::{Base64, DEV_FLAG_GENHASH},
};

#[derive(Debug, Error)]
enum RetryError {
	#[error("No previously connected node found in database")]
	NoPreviousNode,
	#[error("All connection attempts failed: {0}")]
	ConnectionFailed(Report),
	#[error("Retry strategy halted due to shutdown: {0}")]
	Shutdown(String),
}

#[derive(Debug, Error)]
enum ClientCreationError {
	#[error("Failed to create RPC client for host {host}: {error}")]
	ConnectionFailed { host: String, error: Report },

	#[error("Genesis hash mismatch for host {host}. Expected: {expected}, found: {found}")]
	GenesisHashMismatch {
		host: String,
		expected: String,
		found: String,
	},

	#[error("Invalid genesis hash configuration: {0}")]
	InvalidGenesisHash(String),

	#[error("Failed to fetch system version: {0}")]
	SystemVersionError(Report),

	#[error("No available RPC nodes to connect to")]
	NoNodesAvailable,

	#[error("Failed to connect to any RPC node")]
	AllNodesFailed {
		#[source]
		last_error: Report,
	},

	#[error("RPC function execution failed for host {host}: {error}")]
	RpcCallFailed { host: String, error: Report },
}

struct ConnectionAttempt<T> {
	client: SDK,
	node: Node,
	result: T,
}

enum GenesisHash {
	Dev,
	Hash(H256),
}

impl GenesisHash {
	fn from_hex(hex_str: &str) -> Result<Self> {
		if hex_str.starts_with(DEV_FLAG_GENHASH) {
			warn!(
				"Genesis hash configured for development ({}), skipping verification",
				hex_str
			);
			// Return a dummy hash for development
			return Ok(Self::Dev);
		}

		let bytes: [u8; 32] = from_hex(hex_str)
			.map_err(|_| Report::msg(ClientCreationError::InvalidGenesisHash(hex_str.to_string())))?
			.try_into()
			.map_err(|_| {
				Report::msg(ClientCreationError::InvalidGenesisHash(hex_str.to_string()))
			})?;

		Ok(Self::Hash(H256::from(bytes)))
	}

	fn matches(&self, other: &H256) -> bool {
		match self {
			GenesisHash::Dev => true,
			GenesisHash::Hash(hash) => hash.eq(other),
		}
	}
}

#[cfg(not(target_arch = "wasm32"))]
type SubscriptionStream =
	Pin<Box<dyn Stream<Item = Result<Result<Subscription, subxt::error::Error>, Elapsed>> + Send>>;

#[cfg(target_arch = "wasm32")]
type SubscriptionStream =
	Pin<Box<dyn Stream<Item = Result<Subscription, subxt::error::Error>> + Send>>;

#[derive(Clone)]
pub struct Client<T: Database> {
	subxt_client: Arc<RwLock<SDK>>,
	db: T,
	nodes: Nodes,
	retry_config: RetryConfig,
	expected_genesis_hash: String,
	shutdown: Controller<String>,
	event_sender: Sender<RpcEvent>,
}

pub struct SubmitResponse {
	pub block_hash: H256,
	pub hash: H256,
	pub index: u32,
}

impl<D: Database> Client<D> {
	/// Creates new RPC Client
	pub async fn new(
		db: D,
		nodes: Nodes,
		expected_genesis_hash: &str,
		retry_config: RetryConfig,
		shutdown: Controller<String>,
		event_sender: Sender<RpcEvent>,
	) -> Result<Self> {
		let (client, node) = Self::initialize_connection(
			&nodes,
			expected_genesis_hash,
			retry_config.clone(),
			shutdown.clone(),
			event_sender.clone(),
		)
		.await?;

		let client = Self {
			subxt_client: Arc::new(RwLock::new(client)),
			db,
			nodes,
			retry_config,
			expected_genesis_hash: expected_genesis_hash.to_string(),
			shutdown,
			event_sender,
		};

		client.db.put(RpcNodeKey, node);

		Ok(client)
	}

	// Initializes the first connection to the shuffled list of RPC nodes using the provided Retry strategy.
	// The process can be interrupted by the Shutdown signal.
	async fn initialize_connection(
		nodes: &Nodes,
		expected_genesis_hash: &str,
		retry_config: RetryConfig,
		shutdown: Controller<String>,
		event_sender: Sender<RpcEvent>,
	) -> Result<(SDK, Node)> {
		let (available_nodes, _) = nodes.shuffle(Default::default());
		let connection_result = shutdown
			.with_cancel(Retry::spawn(retry_config, || {
				Self::attempt_connection(
					&available_nodes,
					expected_genesis_hash,
					event_sender.clone(),
				)
			}))
			.await;

		match connection_result {
			Ok(Ok(ConnectionAttempt { client, node, .. })) => Ok((client, node)),
			Ok(Err(err)) => Err(Report::msg(RetryError::ConnectionFailed(err))),
			Err(err) => Err(Report::msg(RetryError::Shutdown(err.to_string()))),
		}
	}

	// Attempts to establish the first successful connection from the list of provided Nodes with a matching genesis hash.
	async fn attempt_connection(
		nodes: &[Node],
		expected_genesis_hash: &str,
		event_sender: Sender<RpcEvent>,
	) -> Result<ConnectionAttempt<()>> {
		// Not passing any RPC function calls since this is a first try of connecting RPC nodes
		match Self::try_connect_and_execute(nodes, expected_genesis_hash, |_| {
			futures::future::ok(())
		})
		.await
		{
			Ok(attempt) => {
				// On success, log and send event
				info!(
					"Successfully initialized connection to the RPC host: {}",
					attempt.node.host
				);
				// Output event, signaling first ever initialized connection to the RPC host
				event_sender.send(RpcEvent::InitialConnection(attempt.node.host.clone()))?;
				Ok(attempt)
			},
			Err(err) => Err(err),
		}
	}

	// Iterates through the RPC nodes, tries to create the first successful connection, verifies the genesis hash,
	// and tries to executes the given RPC function.
	async fn try_connect_and_execute<T, F, Fut>(
		nodes: &[Node],
		expected_genesis_hash: &str,
		f: F,
	) -> Result<ConnectionAttempt<T>>
	where
		F: FnMut(SDK) -> Fut + Copy,
		Fut: std::future::Future<Output = Result<T>>,
	{
		if nodes.is_empty() {
			return Err(Report::msg(ClientCreationError::NoNodesAvailable));
		}

		let mut last_error = None;
		for node in nodes {
			match Self::try_node_connection_and_exec(node, expected_genesis_hash, f).await {
				Ok(attempt) => return Ok(attempt),
				Err(err) => {
					warn!(host = %node.host, error = %err, "Failed to connect to RPC node");
					last_error = Some(err);
				},
			}
		}

		Err(Report::msg(ClientCreationError::AllNodesFailed {
			last_error: last_error.unwrap_or_else(|| eyre!("No error recorded")),
		}))
	}

	// Tries to connect to the provided RPC host, verifies the genesis hash,
	// and tries to executes the given RPC function.
	async fn try_node_connection_and_exec<T, F, Fut>(
		node: &Node,
		expected_genesis_hash: &str,
		mut f: F,
	) -> Result<ConnectionAttempt<T>>
	where
		F: FnMut(SDK) -> Fut + Copy,
		Fut: std::future::Future<Output = Result<T>>,
	{
		match Self::create_rpc_client(&node.host, expected_genesis_hash).await {
			Ok((client, node)) => {
				// Execute the provided  RPC function call with the created client
				let result = f(client.clone()).await.map_err(|e| {
					Report::msg(ClientCreationError::RpcCallFailed {
						host: node.host.clone(),
						error: e,
					})
				})?;

				Ok(ConnectionAttempt {
					client,
					node,
					result,
				})
			},
			Err(err) => Err(Report::msg(ClientCreationError::ConnectionFailed {
				host: node.host.clone(),
				error: err,
			})),
		}
	}

	// Creates the RPC client by connecting to the provided RPC host and checks if the provided genesis hash matches the one from the client
	async fn create_rpc_client(host: &str, expected_genesis_hash: &str) -> Result<(SDK, Node)> {
		let subxt_client = SubxtRpcClient::from_insecure_url(host).await?;
		let online_client = AOnlineClient::from_rpc_client(subxt_client.clone()).await?;
		let avail_client = AvailRpcClient::new(online_client, subxt_client);
		let client = SDK::new_custom(avail_client)
			.await
			.map_err(|e| Report::msg(e.to_string()))?;

		// Verify genesis hash
		let genesis_hash = client.client.online_client.genesis_hash();
		info!("Genesis hash for {}: {:?}", host, genesis_hash);

		let expected_hash = GenesisHash::from_hex(expected_genesis_hash)?;

		if !expected_hash.matches(&genesis_hash) {
			return Err(Report::msg(ClientCreationError::GenesisHashMismatch {
				host: host.to_string(),
				expected: expected_genesis_hash.to_string(),
				found: format!("{:?}", genesis_hash),
			}));
		}

		// Fetch system and runtime information
		let system_version = version(&client.client)
			.await
			.map_err(|e| Report::msg(ClientCreationError::SystemVersionError(eyre!("{:?}", e))))?;

		let runtime_version = client.client.online_client.runtime_version();

		// Create Node variant
		let node = Node::new(
			host.to_string(),
			system_version,
			runtime_version.spec_version,
			genesis_hash,
		);

		Ok((client, node))
	}

	// Provides the Retry strategy for the passed RPC function call
	async fn with_retries<F, Fut, T>(&self, f: F) -> Result<T>
	where
		F: FnMut(SDK) -> Fut + Copy,
		Fut: std::future::Future<Output = Result<T>>,
	{
		// First attempt with current client
		if let Ok(result) = self.try_with_current_client(f).await {
			return Ok(result);
		}

		// If current client failed try to reconnect using stored node
		let connected_node = self
			.db
			.get(RpcNodeKey)
			.ok_or(RetryError::NoPreviousNode)
			.map_err(Report::msg)?;

		warn!(
			"Executing RPC call with host: {} failed. Trying to create a new RPC connection.",
			connected_node.host
		);

		// Try new nodes first, then previous nodes if new ones fail
		let (new_nodes, previous_nodes) = self.nodes.shuffle(&connected_node.host);

		if let Ok(result) = self.try_nodes_with_retries(f, &new_nodes).await {
			return Ok(result);
		}

		// Fall back to previous nodes, if new nodes failed
		self.try_nodes_with_retries(f, &previous_nodes).await
	}

	// Tries to execute the provided RPC function call only with the currently connected RPC node
	async fn try_with_current_client<F, Fut, T>(&self, mut f: F) -> Result<T>
	where
		F: FnMut(SDK) -> Fut + Copy,
		Fut: std::future::Future<Output = Result<T>>,
	{
		let retry_result = self
			.shutdown
			.with_cancel(Retry::spawn(
				self.retry_config.clone(),
				move || async move { f(self.current_client().await).await },
			))
			.await;

		match retry_result {
			Ok(Ok(result)) => Ok(result),
			Ok(Err(err)) => Err(Report::msg(RetryError::ConnectionFailed(err))),
			Err(err) => Err(Report::msg(RetryError::Shutdown(err.to_string()))),
		}
	}

	// Iterates through the nodes to find the first successful connection,
	// executes the given RPC function, and retries on failure using the provided Retry strategy.
	async fn try_nodes_with_retries<F, Fut, T>(&self, f: F, nodes: &[Node]) -> Result<T>
	where
		F: FnMut(SDK) -> Fut + Copy,
		Fut: std::future::Future<Output = Result<T>>,
	{
		let nodes_fn = move || async move {
			Self::try_connect_and_execute(nodes, &self.expected_genesis_hash, f).await
		};

		match self
			.shutdown
			.with_cancel(Retry::spawn(self.retry_config.clone(), nodes_fn))
			.await
		{
			Ok(Ok(ConnectionAttempt {
				client,
				node,
				result,
			})) => {
				self.event_sender
					.send(RpcEvent::SwitchedConnection(node.host.clone()))?;
				info!("Switched connection to a new RPC host: {}", node.host);

				*self.subxt_client.write().await = client;
				self.db.put(RpcNodeKey, node);
				Ok(result)
			},
			Ok(Err(err)) => Err(Report::msg(RetryError::ConnectionFailed(err))),
			Err(err) => Err(Report::msg(RetryError::Shutdown(err.to_string()))),
		}
	}

	pub async fn subscription_stream(self) -> impl Stream<Item = Result<Subscription>> {
		async_stream::stream! {
			loop {
				match self.with_retries(Self::create_rpc_subscriptions).await {
					Ok(mut stream) => {
						loop {
							match stream.next().await {
								#[cfg(not(target_arch = "wasm32"))]
								Some(Ok(Ok(item))) => {
									 yield Ok(item);
									 continue;
								},
								#[cfg(target_arch = "wasm32")]
								Some(Ok(item)) => {
									yield Ok(item);
									continue;
								},
								#[cfg(not(target_arch = "wasm32"))]
								Some(Ok(Err(error))) => warn!(%error, "Received error on RPC Subscription stream. Creating new connection."),
								Some(Err(error)) => warn!(%error, "Received error on RPC Subscription stream. Creating new connection."),
								None => warn!("RPC Subscription Stream exhausted. Creating new connection."),
							}
							break;
						}

					}
					Err(err) => {
						warn!(error = %err, "Failed to create RPC Subscription stream.");
						yield Err(err);
						return;
					}
				}
			}
		}
	}

	pub async fn current_client(&self) -> SDK {
		self.subxt_client.read().await.clone()
	}

	async fn create_rpc_subscriptions(client: SDK) -> Result<SubscriptionStream> {
		// NOTE: current tokio stream implementation doesn't support timeouts on web
		#[cfg(not(target_arch = "wasm32"))]
		let timeout_in = Duration::from_secs(30);

		let headers_stream = client
			.client
			.online_client
			.backend()
			.stream_finalized_block_headers()
			.await?
			.map_ok(|(header, _)| Subscription::Header(header))
			.inspect_ok(|_| info!("Received header on the stream"))
			.inspect_err(|error| warn!(%error, "Received error on headers stream"));

		// Create fused Avail Header subscription
		#[cfg(not(target_arch = "wasm32"))]
		let headers: SubscriptionStream = Box::pin(headers_stream.timeout(timeout_in).fuse());
		#[cfg(target_arch = "wasm32")]
		let headers: SubscriptionStream = Box::pin(headers_stream.fuse());

		let justifications_stream = client
			.client
			.rpc_client
			.subscribe(
				"grandpa_subscribeJustifications",
				rpc_params![],
				"grandpa_unsubscribeJustifications",
			)
			.await?
			.map_ok(Subscription::Justification)
			.inspect_ok(|_| info!("Received justification on the stream"))
			.inspect_err(|error| warn!(%error, "Received error on justifications stream"));

		#[cfg(not(target_arch = "wasm32"))]
		// Create fused GrandpaJustification subscription
		let justifications: SubscriptionStream =
			Box::pin(justifications_stream.timeout(timeout_in).fuse());
		#[cfg(target_arch = "wasm32")]
		let justifications: SubscriptionStream = Box::pin(justifications_stream.fuse());

		let mut last_stream = 0;
		let mut per_stream_count = 0;

		let mut streams = StreamMap::new();
		streams.insert(0, Box::pin(headers));
		streams.insert(1, Box::pin(justifications));
		let streams = streams
			.take_while(move |&(stream, _)| {
				if last_stream != stream {
					last_stream = stream;
					per_stream_count = 0;
					return true;
				}

				per_stream_count += 1;

				if per_stream_count < 3 {
					return true;
				}

				// If one of the streams is inactive third time in a row, reconnect
				warn!("Stream stalled, restarting...");
				false
			})
			.map(|(_, value)| value);
		Ok(Box::pin(streams))
	}

	pub async fn get_block_hash(&self, block_number: u32) -> Result<H256> {
		self.with_retries(|client| async move {
			let opt = get_block_hash(&client.client, Some(block_number))
				.await
				.map_err(|e| Report::msg(format!("{e:?}")))?;

			let hash =
				opt.ok_or_else(|| eyre!("no block hash found for block {}", block_number))?;

			Ok(hash)
		})
		.await
	}

	pub async fn get_header_by_hash(&self, block_hash: H256) -> Result<AvailHeader> {
		self.with_retries(|client| async move {
			client
				.client
				.online_client
				.backend()
				.block_header(block_hash)
				.await?
				.ok_or_else(|| {
					subxt::Error::Other(
						format!("Block Header with hash: {block_hash:?} not found",),
					)
				})
				.map_err(Into::into)
		})
		.await
		.wrap_err(format!(
			"Block Header with hash: {:?} not found",
			block_hash
		))
	}

	pub async fn get_validator_set_by_hash(&self, block_hash: H256) -> Result<Vec<Public>> {
		let res = self
			.with_retries(|client| async move {
				client
					.client
					.online_client
					.runtime_api()
					.at(block_hash)
					.call_raw::<Vec<(Public, u64)>>("GrandpaApi_grandpa_authorities", None)
					.await
					.map_err(Into::into)
			})
			.await?
			.iter()
			.map(|e| e.0)
			.collect();

		Ok(res)
	}

	pub async fn get_finalized_head_hash(&self) -> Result<H256> {
		let head = self
			.with_retries(|client| async move {
				get_finalized_head(&client.client)
					.await
					.map_err(|error| subxt::Error::Other(format!("{:?}", error)))
					.map_err(Into::into)
			})
			.await?;

		Ok(head)
	}

	pub async fn get_chain_head_header(&self) -> Result<AvailHeader> {
		let finalized_hash = self.get_finalized_head_hash().await?;
		self.get_header_by_hash(finalized_hash).await
	}

	pub async fn request_kate_rows(&self, rows: Rows, block_hash: H256) -> Result<Vec<Vec<u8>>> {
		self.with_retries(|client| {
			let rows = rows.clone();
			async move {
				let rows = query_rows(&client.client.rpc_client, rows.to_vec(), Some(block_hash)).await?;
				Ok(rows
					.iter()
					.map(|row| {
						row.iter()
							.flat_map(|cell| cell.to_big_endian().to_vec())
							.collect()
					})
					.collect())
			}
		})
		.await
	}

	pub async fn request_kate_proof(
		&self,
		block_hash: H256,
		positions: &[Position],
	) -> Result<Vec<Cell>> {
		fn concat_content(scalar: U256, proof: GProof) -> Result<[u8; 80]> {
			let proof: Vec<u8> = proof.into();
			if proof.len() != 48 {
				return Err(eyre!("Invalid proof length"));
			}

			let mut result = [0u8; 80];
			result[..48].copy_from_slice(&proof);

			let scalar_bytes = scalar.to_big_endian();
			result[48..].copy_from_slice(&scalar_bytes);
			Ok(result)
		}

		let cells: Cells = positions
			.iter()
			.map(|p| avail_rust::Cell {
				row: p.row,
				col: p.col as u32,
			})
			.collect::<Vec<_>>()
			.try_into()
			.map_err(|_| eyre!("Failed to convert to cells"))?;

		let proofs: Vec<(GRawScalar, GProof)> = self
			.with_retries(|client| {
				let cells = cells.clone();
				async move {
					query_proof(&client.client, cells.to_vec(), Some(block_hash))
						.await
						.map_err(Into::into)
				}
			})
			.await?;

		let contents = proofs
			.into_iter()
			.map(|(scalar, proof)| concat_content(scalar, proof).expect("Contents concated"));

		Ok(positions
			.iter()
			.zip(contents)
			.map(|(&position, content)| Cell::SingleCell( SingleCell { position, content }))
			.collect::<Vec<_>>())
	}

	pub async fn get_system_version(&self) -> Result<String> {
		let ver = self
			.with_retries(|client| async move { version(&client.client).await.map_err(Into::into) })
			.await?;

		Ok(ver)
	}

	pub async fn get_runtime_version(&self) -> Result<RuntimeVersion> {
		let ver = self
			.with_retries(|client| async move {
				get_runtime_version(&client.client, None)
					.await
					.map_err(Into::into)
			})
			.await?;

		Ok(ver)
	}

	pub async fn get_validator_set_by_block_number(&self, block_num: u32) -> Result<Vec<Public>> {
		let hash = self.get_block_hash(block_num).await?;
		self.get_validator_set_by_hash(hash).await
	}

	pub async fn fetch_set_id_at(&self, block_hash: H256) -> Result<u64> {
		let res = self
			.with_retries(|client| {
				let set_id_key = avail::storage().grandpa().current_set_id();
				async move {
					client
						.client
						.storage()
						.at(block_hash)
						.fetch(&set_id_key)
						.await
						.map_err(Into::into)
				}
			})
			.await?
			.ok_or_else(|| eyre!("The set_id should exist"))?;

		Ok(res)
	}

	pub async fn get_current_set_id_by_block_number(&self, block_num: u32) -> Result<u64> {
		let hash = self.get_block_hash(block_num).await?;
		self.fetch_set_id_at(hash).await
	}

	pub async fn get_header_by_block_number(&self, block_num: u32) -> Result<(AvailHeader, H256)> {
		let hash = self.get_block_hash(block_num).await?;
		self.get_header_by_hash(hash)
			.await
			.map(|header| (header, hash))
	}

	pub async fn get_validator_set_at(&self, block_hash: H256) -> Result<Option<Vec<AccountId32>>> {
		let res = self
			.with_retries(|client| {
				let validators_key = avail::storage().session().validators();
				async move {
					client
						.client
						.storage()
						.at(block_hash)
						.fetch(&validators_key)
						.await
						.map_err(Into::into)
				}
			})
			.await?;

		Ok(res)
	}

	pub async fn submit_signed_and_wait_for_finalized(
		&self,
		data: Base64,
		signer: &Keypair,
		app_id: AppId,
	) -> Result<SubmitResponse> {
		let data = Arc::new(data);
		self.with_retries(|client| {
			let data = data.0.clone();
			async move {
				let nonce = self.db.get(SignerNonceKey).unwrap_or(0);
				let options = Options::new().nonce(nonce).app_id(app_id.0);
				let tx = client.tx.data_availability.submit_data(data);

				let data_submission = tx.execute_and_watch_inclusion(signer, options).await;

				let submit_response = match data_submission {
					Ok(success) => Ok(SubmitResponse {
						block_hash: success.block_hash,
						hash: success.tx_hash,
						index: success.tx_index,
					}),
					Err(error) => Err(eyre!("{:?}", error)),
				};

				self.db.put(SignerNonceKey, nonce + 1);

				submit_response
			}
		})
		.await
	}

	pub async fn submit_from_bytes_and_wait_for_finalized(
		&self,
		tx_bytes: Vec<u8>,
	) -> Result<SubmitResponse> {
		self.with_retries(|client| {
			let extrinsic =
				SubmittableExtrinsic::from_bytes(client.client.online_client, tx_bytes.clone());
			async move {
				let tx_in_block = extrinsic
					.submit_and_watch()
					.await?
					.wait_for_finalized()
					.await?;

				tx_in_block
					.wait_for_success()
					.await
					.map_err(Into::into)
					.map(|success| SubmitResponse {
						block_hash: tx_in_block.block_hash(),
						hash: success.extrinsic_hash(),
						index: success.extrinsic_index(),
					})
			}
		})
		.await
	}

	pub async fn get_paged_storage_keys(
		&self,
		key: Vec<u8>,
		count: usize,
		hash: H256,
	) -> Result<Vec<StorageKey>> {
		let key = &key;
		self.with_retries(|client| async move {
			let storage = client.client.storage().at(hash);
			let raw_keys = storage.fetch_raw_keys(key.to_vec()).await?;
			raw_keys
				.take(count)
				.collect::<Result<Vec<_>, _>>()
				.await
				.map_err(Into::into)
		})
		.await
	}

	pub async fn get_session_key_owner_at(
		&self,
		block_hash: H256,
		public_key: Public,
	) -> Result<Option<AccountId32>> {
		let res = self
			.with_retries(|client| {
				let session_key_key_owner = avail::storage()
					.session()
					.key_owner(KeyTypeId(crypto::key_types::GRANDPA.0), public_key.0);
				async move {
					client
						.client
						.storage()
						.at(block_hash)
						.fetch(&session_key_key_owner)
						.await
						.map_err(Into::into)
				}
			})
			.await?;

		Ok(res)
	}

	pub async fn request_finality_proof(&self, block_number: u32) -> Result<WrappedProof> {
		let params = rpc_params![block_number]
			.build()
			.map(|value| value.get().to_string());
		let params = params.as_ref().map(String::as_bytes);
		let res: WrappedProof = self
			.with_retries(|client| async move {
				let api = client
					.client
					.online_client
					.runtime_api()
					.at_latest()
					.await?;
				api.call_raw("grandpa_proveFinality", params)
					.await
					.map_err(Into::into)
			})
			.await
			.map_err(|e| eyre!("Request failed at Finality Proof. Error: {e}"))?;

		Ok(res)
	}

	pub async fn get_genesis_hash(&self) -> Result<H256> {
		let gen_hash = self
			.current_client()
			.await
			.client
			.online_client
			.genesis_hash();

		Ok(gen_hash)
	}
}
