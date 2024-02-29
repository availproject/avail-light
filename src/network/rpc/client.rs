use avail_subxt::{
	api::{self, runtime_types::sp_core::crypto::KeyTypeId},
	avail::{self, Pair},
	build_client,
	primitives::Header,
	utils::H256,
	AvailConfig,
};
use color_eyre::{eyre::eyre, Report, Result};
use futures::{Stream, TryFutureExt, TryStreamExt};
use kate_recovery::{data::Cell, matrix::Position};
use sp_core::{
	bytes::from_hex,
	ed25519::{self, Public},
};
use std::sync::{Arc, Mutex};
use subxt::{
	rpc::{types::BlockNumber, RpcParams},
	rpc_params,
	storage::StorageKey,
	tx::{PairSigner, SubmittableExtrinsic},
	utils::AccountId32,
};
use tokio::sync::RwLock;
use tokio_retry::Retry;
use tokio_stream::StreamExt;
use tracing::{info, warn};

use super::{Node, Nodes, Subscription, WrappedProof, CELL_WITH_PROOF_SIZE};
use crate::{
	consts::ExpectedNodeVariant,
	types::{RetryConfig, RuntimeVersion, State, DEV_FLAG_GENHASH},
};

#[derive(Clone)]
pub struct Client {
	subxt_client: Arc<RwLock<avail::Client>>,
	state: Arc<Mutex<State>>,
	nodes: Nodes,
	retry_config: RetryConfig,
	expected_genesis_hash: String,
}

impl Client {
	pub async fn new(
		state: Arc<Mutex<State>>,
		nodes: Nodes,
		expected_genesis_hash: &str,
		retry_config: RetryConfig,
	) -> Result<Self> {
		// try and connect appropriate Node from the provided list
		// will do retries with the provided Retry Config
		let (client, node, _) = Retry::spawn(retry_config.clone(), || async {
			Self::try_connect_and_execute(
				nodes.shuffle(Default::default()),
				ExpectedNodeVariant::new(),
				expected_genesis_hash,
				|_| futures::future::ok(()),
			)
			.await
		})
		.await?;

		// update application wide State with the newly connected Node
		state.lock().unwrap().connected_node = node;

		Ok(Self {
			subxt_client: Arc::new(RwLock::new(client)),
			state,
			nodes,
			retry_config,
			expected_genesis_hash: expected_genesis_hash.to_string(),
		})
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
				"Expected Node system version:{:?}/{}, found: {}/{}. Skipping to another node.",
				expected_node.system_version,
				expected_node.spec_name,
				system_version,
				runtime_version.spec_name,
			));
		}

		let variant = Node::new(
			host.to_string(),
			system_version,
			runtime_version.spec_name,
			runtime_version.spec_version,
			genesis_hash,
		);

		Ok((client, variant))
	}

	async fn try_connect_and_execute<T, F, Fut>(
		nodes: Vec<Node>,
		expected_node: ExpectedNodeVariant,
		expected_genesis_hash: &str,
		mut f: F,
	) -> Result<(avail::Client, Node, T)>
	where
		F: FnMut(avail::Client) -> Fut + Copy,
		Fut: std::future::Future<Output = Result<T>>,
	{
		// go through the provided list of Nodes to try and find and appropriate one,
		// after a successful connection, try to execute passed function call
		for Node { host, .. } in nodes.iter() {
			let result =
				Self::create_subxt_client(host, expected_node.clone(), expected_genesis_hash)
					.and_then(move |(client, node)| {
						f(client.clone()).map_ok(|res| (client, node, res))
					})
					.await;

			match result {
				Err(error) => warn!(host, %error, "Skipping connection with this node"),
				ok => return ok,
			}
		}

		Err(eyre!("Failed to connect any appropriate working node"))
	}

	async fn with_retries<F, Fut, T>(&self, mut f: F) -> Result<T>
	where
		F: FnMut(avail::Client) -> Fut + Copy,
		Fut: std::future::Future<Output = Result<T, subxt::error::Error>>,
	{
		// try and execute the passed function, use the Retry strategy if needed
		if let Ok(result) = Retry::spawn(self.retry_config.clone(), move || async move {
			f(self.current_client().await).await
		})
		.await
		{
			// this was successful, return early
			return Ok(result);
		}
		// if not, find another Node where this could still be done
		let connected_node = self.state.lock().unwrap().connected_node.clone();
		warn!(
			"Executing RPC call with host: {} failed. Trying to create a new RPC connection.",
			connected_node.host
		);
		// shuffle nodes, if possible
		let nodes = self.nodes.shuffle(connected_node.host);
		// go through available Nodes, try to connect, Retry connecting if needed
		let (client, node, result) = Retry::spawn(self.retry_config.clone(), move || {
			let nodes = nodes.clone();
			async move {
				Self::try_connect_and_execute(
					nodes,
					ExpectedNodeVariant::new(),
					&self.expected_genesis_hash,
					move |client| f(client).map_err(Report::from),
				)
				.await
			}
		})
		.await?;

		// retries gave results, update currently connected Node and created Client
		*self.subxt_client.write().await = client;
		self.state.lock().unwrap().connected_node = node;

		Ok(result)
	}

	async fn create_subxt_subscriptions(
		client: avail::Client,
	) -> Result<impl Stream<Item = Result<Subscription, subxt::error::Error>>, subxt::error::Error>
	{
		// create Header subscription
		let header_subscription = client.rpc().subscribe_finalized_block_headers().await?;
		// map Header subscription to the same type for later matching
		let headers = header_subscription.map_ok(Subscription::Header);

		let justification_subscription = client
			.rpc()
			.subscribe(
				"grandpa_subscribeJustifications",
				rpc_params![],
				"grandpa_unsubscribeJustifications",
			)
			.await?;
		// map Justification subscription to the same type for later matching
		let justifications = justification_subscription.map_ok(Subscription::Justification);

		Ok(headers.merge(justifications))
	}

	pub async fn subscription_stream(self) -> impl Stream<Item = Result<Subscription>> {
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
					// no more subscriptions left on stream, we have to try and create a new stream
					let Some(result) = stream.next().await else {
						warn!("No more items on Subscriptions Stream. Trying to create a new one.");
						continue 'outer
					};
					match result {
						Ok(item) => yield Ok(item),
						// if Error was received, we need to switch to another RPC Client
						Err(err)=> {
							warn!(%err, "Received Error on stream. Trying to create a new one.");
							continue 'outer
						}
					}
				}
			}
		}
	}

	pub async fn current_client(&self) -> avail::Client {
		self.subxt_client.read().await.clone()
	}

	pub async fn get_block_hash(&self, block_number: u32) -> Result<H256> {
		let hash = self
			.with_retries(|client| async move {
				client
					.rpc()
					.block_hash(Some(BlockNumber::from(block_number)))
					.await
			})
			.await?
			.ok_or_else(|| eyre!("Block with number: {} not found", block_number))?;

		Ok(hash)
	}

	pub async fn get_header_by_hash(&self, block_hash: H256) -> Result<Header> {
		let header = self
			.with_retries(|client| async move { client.rpc().header(Some(block_hash)).await })
			.await?
			.ok_or_else(|| eyre!("Block Header with hash: {:?} not found", block_hash))?;

		Ok(header)
	}

	pub async fn get_validator_set_by_hash(&self, block_hash: H256) -> Result<Vec<Public>> {
		let res = self
			.with_retries(|client| async move {
				client
					.runtime_api()
					.at(block_hash)
					.call_raw::<Vec<(Public, u64)>>("GrandpaApi_grandpa_authorities", None)
					.await
			})
			.await?
			.iter()
			.map(|e| e.0)
			.collect();

		Ok(res)
	}

	pub async fn get_finalized_head_hash(&self) -> Result<H256> {
		let head = self
			.with_retries(|client| async move { client.rpc().finalized_head().await })
			.await?;

		Ok(head)
	}

	pub async fn get_chain_head_header(&self) -> Result<Header> {
		let finalized_hash = self.get_finalized_head_hash().await?;
		self.get_header_by_hash(finalized_hash).await
	}

	pub async fn request_kate_rows(
		&self,
		rows: Vec<u32>,
		block_hash: H256,
	) -> Result<Vec<Option<Vec<u8>>>> {
		let mut params = RpcParams::new();
		params.push(rows.clone())?;
		params.push(block_hash)?;

		let res: Vec<Option<Vec<u8>>> = self
			.with_retries(|client| {
				let params = params.clone();
				async move { client.rpc().request("kate_queryRows", params).await }
			})
			.await?;

		Ok(res)
	}

	pub async fn request_kate_proof(
		&self,
		block_hash: H256,
		positions: &[Position],
	) -> Result<Vec<Cell>> {
		let mut params = RpcParams::new();
		params.push(positions)?;
		params.push(block_hash)?;

		let proofs: Vec<u8> = self
			.with_retries(|client| {
				let params = params.clone();
				async move { client.rpc().request("kate_queryProof", params).await }
			})
			.await?;

		let i = proofs
			.chunks_exact(CELL_WITH_PROOF_SIZE)
			.map(|chunk| chunk.try_into().expect("chunks of 80 bytes size"));

		let proof = positions
			.iter()
			.zip(i)
			.map(|(&position, &content)| Cell { position, content })
			.collect::<Vec<_>>();

		Ok(proof)
	}

	pub async fn get_system_version(&self) -> Result<String> {
		let res = self
			.with_retries(|client| async move { client.rpc().system_version().await })
			.await?;

		Ok(res)
	}

	pub async fn get_runtime_version(&self) -> Result<RuntimeVersion> {
		let res: RuntimeVersion = self
			.with_retries(|client| async move {
				client
					.rpc()
					.request("state_getRuntimeVersion", RpcParams::new())
					.await
			})
			.await?;

		Ok(res)
	}

	pub async fn get_validator_set_by_block_number(&self, block_num: u32) -> Result<Vec<Public>> {
		let hash = self.get_block_hash(block_num).await?;
		self.get_validator_set_by_hash(hash).await
	}

	pub async fn fetch_set_id_at(&self, block_hash: H256) -> Result<u64> {
		let res = self
			.with_retries(|client| {
				let set_id_key = api::storage().grandpa().current_set_id();
				async move { client.storage().at(block_hash).fetch(&set_id_key).await }
			})
			.await?
			.ok_or_else(|| eyre!("The set_id should exist"))?;

		Ok(res)
	}

	pub async fn get_current_set_id_by_block_number(&self, block_num: u32) -> Result<u64> {
		let hash = self.get_block_hash(block_num).await?;
		self.fetch_set_id_at(hash).await
	}

	pub async fn get_header_by_block_number(&self, block_num: u32) -> Result<(Header, H256)> {
		let hash = self.get_block_hash(block_num).await?;
		self.get_header_by_hash(hash)
			.await
			.map(|header| (header, hash))
	}

	pub async fn get_validator_set_at(&self, block_hash: H256) -> Result<Option<Vec<AccountId32>>> {
		let res = self
			.with_retries(|client| {
				let validators_key = api::storage().session().validators();
				async move { client.storage().at(block_hash).fetch(&validators_key).await }
			})
			.await
			.map_err(Report::from)?;

		Ok(res)
	}

	pub async fn submit_signed_and_wait_for_finalized<Call: subxt::tx::TxPayload>(
		&self,
		call: &Call,
		signer: &PairSigner<AvailConfig, Pair>,
		other_params: avail_subxt::primitives::AvailExtrinsicParams,
	) -> Result<subxt::blocks::ExtrinsicEvents<AvailConfig>> {
		let tx_progress = self
			.with_retries(|client| {
				let other_params = other_params.clone();
				async move {
					client
						.tx()
						.sign_and_submit_then_watch(call, signer, other_params)
						.await
				}
			})
			.await?;

		tx_progress
			.wait_for_finalized_success()
			.await
			.map_err(Report::from)
	}

	pub async fn submit_from_bytes_and_wait_for_finalized(
		&self,
		tx_bytes: Vec<u8>,
	) -> Result<subxt::blocks::ExtrinsicEvents<AvailConfig>> {
		let tx_progress = self
			.with_retries(|client| {
				let tx_bytes = tx_bytes.clone();
				async move {
					SubmittableExtrinsic::from_bytes(client, tx_bytes)
						.submit_and_watch()
						.await
				}
			})
			.await?;

		let ext = tx_progress
			.wait_for_finalized_success()
			.await
			.map_err(Report::from)?;

		Ok(ext)
	}

	pub async fn get_paged_storage_keys(
		&self,
		key: Vec<u8>,
		count: u32,
		start_key: Option<Vec<u8>>,
		hash: Option<H256>,
	) -> Result<Vec<StorageKey>> {
		let res = self
			.with_retries(|client| {
				let key = &key;
				let start_key = start_key.clone();
				async move {
					client
						.rpc()
						.storage_keys_paged(key, count, start_key.as_deref(), hash)
						.await
				}
			})
			.await
			.map_err(Report::from)?;

		Ok(res)
	}

	pub async fn get_session_key_owner_at(
		&self,
		block_hash: H256,
		public_key: ed25519::Public,
	) -> Result<Option<AccountId32>> {
		let res = self
			.with_retries(|client| {
				let session_key_key_owner = api::storage().session().key_owner(
					KeyTypeId(sp_core::crypto::key_types::GRANDPA.0),
					public_key.0,
				);
				async move {
					client
						.storage()
						.at(block_hash)
						.fetch(&session_key_key_owner)
						.await
				}
			})
			.await
			.map_err(Report::from)?;

		Ok(res)
	}

	pub async fn request_finality_proof(&self, block_number: u32) -> Result<WrappedProof> {
		let mut params = RpcParams::new();
		params.push(block_number)?;

		let res: WrappedProof = self
			.with_retries(|client| {
				let params = params.clone();
				async move { client.rpc().request("grandpa_proveFinality", params).await }
			})
			.await
			.map_err(|e| eyre!("Request failed at Finality Proof. Error: {e}"))?;

		Ok(res)
	}

	pub async fn get_genesis_hash(&self) -> Result<H256> {
		let gen_hash = self.current_client().await.genesis_hash();

		Ok(gen_hash)
	}
}
