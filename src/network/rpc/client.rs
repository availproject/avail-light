use avail_core::AppId;
use avail_subxt::{
	api::{self, runtime_types::sp_core::crypto::KeyTypeId},
	avail::{Cells, GProof, GRawScalar, Rows},
	primitives::Header,
	rpc::KateRpcClient,
	submit::submit_data,
	tx,
	utils::H256,
	AvailClient, AvailConfig,
};
use color_eyre::{
	eyre::{eyre, Context},
	Report, Result,
};
use futures::{Stream, TryFutureExt, TryStreamExt};
use kate_recovery::{data::Cell, matrix::Position};
use sp_core::{bytes::from_hex, ed25519::Public, U256};
use std::sync::Arc;
use subxt::{
	backend::{
		legacy::rpc_methods::{BlockNumber, StorageKey},
		RuntimeVersion,
	},
	rpc_params,
	tx::SubmittableExtrinsic,
	utils::AccountId32,
};
use subxt_signer::sr25519::Keypair;
use tokio::sync::RwLock;
use tokio_retry::Retry;
use tokio_stream::StreamExt;
use tracing::{info, warn};

use super::{Node, Nodes, Subscription, WrappedProof};
use crate::{
	api::v2::types::Base64,
	consts::ExpectedNodeVariant,
	data::{Database, RpcNodeKey},
	shutdown::Controller,
	types::{RetryConfig, DEV_FLAG_GENHASH},
};

#[derive(Clone)]
pub struct Client<T: Database> {
	subxt_client: Arc<RwLock<Arc<AvailClient>>>,
	db: T,
	nodes: Nodes,
	retry_config: RetryConfig,
	expected_genesis_hash: String,
	shutdown: Controller<String>,
}

impl<D: Database> Client<D> {
	pub async fn new(
		db: D,
		nodes: Nodes,
		expected_genesis_hash: &str,
		retry_config: RetryConfig,
		shutdown: Controller<String>,
	) -> Result<Self> {
		// try and connect appropriate Node from the provided list
		// will do retries with the provided Retry Config
		let (client, node, _) = match shutdown
			.with_cancel(Retry::spawn(retry_config.clone(), || async {
				Self::try_connect_and_execute(
					nodes.shuffle(Default::default()),
					ExpectedNodeVariant::default(),
					expected_genesis_hash,
					|_| futures::future::ok(()),
				)
				.await
			}))
			.await
		{
			Ok(result) => result?,
			Err(err) => {
				return Err(eyre!(
					"RPC Client creation Retry strategy halted due to shutdown: {err}"
				))
			},
		};

		// update application wide State with the newly connected Node
		// get the currently persisted node from DB implementation
		_ = db.put(RpcNodeKey, node);

		Ok(Self {
			subxt_client: Arc::new(RwLock::new(client)),
			db,
			nodes,
			retry_config,
			expected_genesis_hash: expected_genesis_hash.to_string(),
			shutdown,
		})
	}

	async fn create_subxt_client(
		host: &str,
		expected_node: ExpectedNodeVariant,
		expected_genesis_hash: &str,
	) -> Result<(AvailClient, Node)> {
		let client = AvailClient::new_insecure(host)
			.await
			.map_err(|e| eyre!(e))?;

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
		let system_version = client.legacy_rpc().system_version().await?;
		let runtime_version: RuntimeVersion = client.runtime_version();

		if !expected_node.matches(&system_version) {
			return Err(eyre!(
				"Expected Node system version:{:?}, found: {}. Skipping to another node.",
				expected_node.system_version,
				system_version,
			));
		}

		let variant = Node::new(
			host.to_string(),
			system_version,
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
	) -> Result<(Arc<AvailClient>, Node, T)>
	where
		F: FnMut(Arc<AvailClient>) -> Fut + Copy,
		Fut: std::future::Future<Output = Result<T>>,
	{
		// go through the provided list of Nodes to try and find and appropriate one,
		// after a successful connection, try to execute passed function call
		for Node { host, .. } in nodes.iter() {
			let result =
				Self::create_subxt_client(host, expected_node.clone(), expected_genesis_hash)
					.and_then(move |(client, node)| {
						let client = Arc::new(client);
						f(client.clone()).map_ok(move |res| (client, node, res))
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
		F: FnMut(Arc<AvailClient>) -> Fut + Copy,
		Fut: std::future::Future<Output = Result<T>>,
	{
		// try and execute the passed function, use the Retry strategy if needed
		match self
			.shutdown
			.with_cancel(Retry::spawn(
				self.retry_config.clone(),
				move || async move { f(self.current_client().await).await },
			))
			.await
		{
			// this was successful, return early
			Ok(Ok(result)) => return Ok(result),
			// if there was an error, skip ahead and try to find a new Node
			Ok(Err(_)) => {},
			// shutdown happened, stop everything
			Err(err) => {
				return Err(eyre!(
					"RPC Client call Retry Strategy halted due to shutdown: {err}"
				))
			},
		}
		// if retries were not successful, find another Node where this could still be done
		if let Some(connected_node) = self.db.get(RpcNodeKey)? {
			warn!(
				"Executing RPC call with host: {} failed. Trying to create a new RPC connection.",
				connected_node.host
			);

			// shuffle nodes, if possible
			let nodes = self.nodes.shuffle(connected_node.host);
			// go through available Nodes, try to connect, Retry connecting if needed
			let (client, node, result) = match self
				.shutdown
				.with_cancel(Retry::spawn(self.retry_config.clone(), || async {
					let nodes = nodes.clone();
					Self::try_connect_and_execute(
						nodes,
						ExpectedNodeVariant::default(),
						&self.expected_genesis_hash,
						move |client| f(client).map_err(Report::from),
					)
					.await
				}))
				.await
			{
				Ok(res) => res?,
				Err(err) => {
					return Err(eyre!(
					"RPC Node selection for passed Client calls' Retry Strategy halted due to shutdown: {err}"
				))
				},
			};

			// retries gave results,
			// update db with currently connected Node and keep a reference to the created Client
			*self.subxt_client.write().await = client;
			_ = self.db.put(RpcNodeKey, node);

			return Ok(result);
		}

		Err(eyre!(
			"Couldn't find a persisted Node that was connected previously."
		))
	}

	async fn create_subxt_subscriptions(
		client: Arc<AvailClient>,
	) -> Result<impl Stream<Item = Result<Subscription, subxt::error::Error>>> {
		// create Header subscription
		let header_subscription = client.backend().stream_finalized_block_headers().await?;
		// map Header subscription to the same type for later matching
		let headers = header_subscription.map_ok(|(header, _)| Subscription::Header(header));

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
					Self::create_subxt_subscriptions(client)
						.await
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

	pub async fn current_client(&self) -> Arc<AvailClient> {
		self.subxt_client.read().await.clone()
	}

	pub async fn get_block_hash(&self, block_number: u32) -> Result<H256> {
		let hash = self
			.with_retries(|client| async move {
				client
					.legacy_rpc()
					.chain_get_block_hash(Some(BlockNumber::from(block_number)))
					.await
					.map_err(Into::into)
			})
			.await?
			.ok_or_else(|| eyre!("Block with number: {} not found", block_number))?;

		Ok(hash)
	}

	pub async fn get_header_by_hash(&self, block_hash: H256) -> Result<Header> {
		self.with_retries(|client| async move {
			client
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
				client
					.legacy_rpc()
					.chain_get_finalized_head()
					.await
					.map_err(Into::into)
			})
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
	) -> Result<Vec<Vec<u8>>> {
		let rows = Rows::try_from(rows).unwrap();
		self.with_retries(|client| {
			let rows = rows.clone();
			async move {
				let rows = client
					.rpc_methods()
					.query_rows(rows, block_hash)
					.await
					.map_err(|error| subxt::Error::Other(format!("{error}")))?;
				Ok(rows
					.iter()
					.map(|row| {
						row.iter()
							.flat_map(|cell| {
								let mut bytes = [0u8; 32];
								cell.to_big_endian(&mut bytes);
								bytes.to_vec()
							})
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
			scalar.to_big_endian(&mut result[48..]);
			result[..48].copy_from_slice(&proof);
			Ok(result)
		}

		let cells: Cells = positions
			.iter()
			.map(|p| avail_subxt::Cell {
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
					client
						.rpc_methods()
						.query_proof(cells, block_hash)
						.await
						.map_err(|error| subxt::Error::Other(format!("{error}")))
						.map_err(Into::into)
				}
			})
			.await
			.map_err(Report::from)?;

		let contents = proofs
			.into_iter()
			.map(|(scalar, proof)| concat_content(scalar, proof).expect("TODO"));

		Ok(positions
			.iter()
			.zip(contents)
			.map(|(&position, content)| Cell { position, content })
			.collect::<Vec<_>>())
	}

	pub async fn get_system_version(&self) -> Result<String> {
		let res = self
			.with_retries(|client| async move {
				client
					.legacy_rpc()
					.system_version()
					.await
					.map_err(Into::into)
			})
			.await?;

		Ok(res)
	}

	pub async fn get_runtime_version(&self) -> Result<RuntimeVersion> {
		self.with_retries(|client| async move { Ok(client.runtime_version()) })
			.await
	}

	pub async fn get_validator_set_by_block_number(&self, block_num: u32) -> Result<Vec<Public>> {
		let hash = self.get_block_hash(block_num).await?;
		self.get_validator_set_by_hash(hash).await
	}

	pub async fn fetch_set_id_at(&self, block_hash: H256) -> Result<u64> {
		let res = self
			.with_retries(|client| {
				let set_id_key = api::storage().grandpa().current_set_id();
				async move {
					client
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
				async move {
					client
						.storage()
						.at(block_hash)
						.fetch(&validators_key)
						.await
						.map_err(Into::into)
				}
			})
			.await
			.map_err(Report::from)?;

		Ok(res)
	}

	pub async fn submit_signed_and_wait_for_finalized(
		&self,
		data: Base64,
		signer: &Keypair,
		app_id: AppId,
	) -> Result<subxt::blocks::ExtrinsicEvents<AvailConfig>> {
		let data = Arc::new(data);
		self.with_retries(|client| {
			let data = data.clone();
			async move {
				tx::in_finalized(submit_data(&client, signer, data.as_ref(), app_id).await?)
					.await?
					.wait_for_success()
					.await
					.map_err(Into::into)
			}
		})
		.await
	}

	pub async fn submit_from_bytes_and_wait_for_finalized(
		&self,
		tx_bytes: Vec<u8>,
	) -> Result<subxt::blocks::ExtrinsicEvents<AvailConfig>> {
		self.with_retries(|client| {
			let extrinsic =
				SubmittableExtrinsic::from_bytes(client.online().clone(), tx_bytes.clone());
			async move {
				tx::in_finalized(extrinsic.submit_and_watch().await?)
					.await?
					.wait_for_success()
					.await
					.map_err(Into::into)
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
			let storage = client.storage().at(hash);
			let raw_keys = storage.fetch_raw_keys(key.to_vec()).await?;
			raw_keys
				.take(count)
				.collect::<Result<Vec<_>, _>>()
				.await
				.map_err(Into::into)
		})
		.await
		.map_err(Report::from)
	}

	pub async fn get_session_key_owner_at(
		&self,
		block_hash: H256,
		public_key: Public,
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
						.map_err(Into::into)
				}
			})
			.await
			.map_err(Report::from)?;

		Ok(res)
	}

	pub async fn request_finality_proof(&self, block_number: u32) -> Result<WrappedProof> {
		let params = rpc_params![block_number]
			.build()
			.map(|value| value.get().to_string());
		let params = params.as_ref().map(String::as_bytes);
		let res: WrappedProof = self
			.with_retries(|client| async move {
				let api = client.runtime_api().at_latest().await?;
				api.call_raw("grandpa_proveFinality", params)
					.await
					.map_err(Into::into)
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
