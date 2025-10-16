use async_trait::async_trait;
use avail_core::kate::COMMITMENT_SIZE;
use avail_rust::H256;
use clap::ValueEnum;
use color_eyre::{eyre::WrapErr, Result};
use kate_recovery::{
	commons::ArkPublicParams,
	data::Cell,
	matrix::{Dimensions, Position},
};
use libp2p::{Multiaddr, PeerId};
use mockall::automock;
use p2p::configuration::Listeners;
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
use std::{str::FromStr, sync::Arc};
use strum::Display;
#[cfg(not(target_arch = "wasm32"))]
use tokio::{sync::Mutex, time::Instant};
#[cfg(target_arch = "wasm32")]
use tokio_with_wasm::alias::sync::Mutex;
use tracing::{debug, info};
#[cfg(target_arch = "wasm32")]
use web_time::{Duration, Instant};

use crate::{data::Database, proof, types::NetworkMode};

pub mod p2p;
pub mod rpc;

#[async_trait]
#[automock]
pub trait Client {
	async fn fetch_verified(
		&self,
		block_number: u32,
		block_hash: H256,
		dimensions: Dimensions,
		commitments: &[[u8; COMMITMENT_SIZE]],
		positions: &[Position],
	) -> Result<(Vec<Cell>, Vec<Position>, FetchStats)>;
}

pub struct FetchStats {
	pub dht_fetched: f64,
	pub dht_fetched_percentage: f64,
	pub dht_fetch_duration: f64,
	pub rpc_fetched: Option<f64>,
	pub rpc_fetch_duration: Option<f64>,
}

type RPCFetchStats = (usize, Duration);

impl FetchStats {
	pub fn new(
		total: usize,
		dht_fetched: usize,
		dht_fetch_duration: Duration,
		rpc_fetch_stats: Option<RPCFetchStats>,
	) -> Self {
		FetchStats {
			dht_fetched: dht_fetched as f64,
			dht_fetched_percentage: dht_fetched as f64 / total as f64,
			dht_fetch_duration: dht_fetch_duration.as_secs_f64(),
			rpc_fetched: rpc_fetch_stats.map(|(rpc_fetched, _)| rpc_fetched as f64),
			rpc_fetch_duration: rpc_fetch_stats.map(|(_, duration)| duration.as_secs_f64()),
		}
	}
}

struct DHTWithRPCFallbackClient<T: Database> {
	p2p_client: Arc<Mutex<Option<p2p::Client>>>,
	rpc_client: rpc::Client<T>,
	pp: Arc<ArkPublicParams>,
	network_mode: NetworkMode,
	insert_into_dht: bool,
}

type Commitments = [[u8; COMMITMENT_SIZE]];

impl<T: Database> DHTWithRPCFallbackClient<T> {
	async fn fetch_verified_from_dht(
		&self,
		block_number: u32,
		dimensions: Dimensions,
		commitments: &Commitments,
		positions: &[Position],
	) -> Result<(Vec<Cell>, Vec<Position>, Duration)> {
		let begin = Instant::now();

		// If p2p_client is not available, return empty cells and all positions as unfetched
		let p2p_client = {
			let client_guard = self.p2p_client.lock().await;
			if let Some(client) = client_guard.as_ref() {
				client.clone()
			} else {
				debug!(
					block_number,
					cells_total = positions.len(),
					"P2P client not available, skipping DHT fetch"
				);
				return Ok((Vec::new(), positions.to_vec(), Duration::from_secs(0)));
			}
		};

		let (mut dht_fetched, mut unfetched) = p2p_client
			.fetch_cells_from_dht(block_number, positions)
			.await;

		let fetch_elapsed = begin.elapsed();
		let (verified, mut unverified) = proof::verify(
			block_number,
			dimensions,
			&dht_fetched,
			commitments,
			self.pp.clone(),
		)
		.await
		.context("Failed to verify fetched cells")?;

		info!(
			block_number,
			cells_total = positions.len(),
			cells_fetched = dht_fetched.len(),
			cells_verified = verified.len(),
			fetch_elapsed = ?fetch_elapsed,
			proof_verification_elapsed = ?(begin.elapsed() - fetch_elapsed),
			"Cells fetched from DHT"
		);

		dht_fetched.retain(|cell| verified.contains(&cell.position()));
		unfetched.append(&mut unverified);

		Ok((dht_fetched, unfetched, fetch_elapsed))
	}

	async fn fetch_verified_from_rpc(
		&self,
		block_number: u32,
		block_hash: H256,
		dimensions: Dimensions,
		commitments: &Commitments,
		positions: &[Position],
	) -> Result<(Vec<Cell>, Vec<Position>, Duration)> {
		let begin = Instant::now();

		let mut fetched: Vec<Cell> = {
			#[cfg(not(feature = "multiproof"))]
			{
				self.rpc_client
					.request_kate_proof(block_hash, positions)
					.await?
					.into_iter()
					.map(Cell::SingleCell)
					.collect()
			}

			#[cfg(feature = "multiproof")]
			{
				self.rpc_client
					.request_kate_multi_proof(block_hash, positions)
					.await?
					.into_iter()
					.map(Cell::MultiProofCell)
					.collect()
			}
		};

		let fetch_elapsed = begin.elapsed();
		let (verified, unverified) = proof::verify(
			block_number,
			dimensions,
			&fetched,
			commitments,
			self.pp.clone(),
		)
		.await
		.context("Failed to verify fetched cells")?;

		info!(
			block_number,
			cells_total = positions.len(),
			cells_fetched = fetched.len(),
			cells_verified = verified.len(),
			fetch_elapsed = ?fetch_elapsed,
			proof_verification_elapsed = ?(begin.elapsed() - fetch_elapsed),
			"Cells fetched from RPC"
		);

		fetched.retain(|cell| verified.contains(&cell.position()));
		Ok((fetched, unverified, fetch_elapsed))
	}
}

#[async_trait]
impl<T: Database + Sync> Client for DHTWithRPCFallbackClient<T> {
	async fn fetch_verified(
		&self,
		block_number: u32,
		block_hash: H256,
		dimensions: Dimensions,
		commitments: &Commitments,
		positions: &[Position],
	) -> Result<(Vec<Cell>, Vec<Position>, FetchStats)> {
		// Skip P2P retrieval in RPC-only mode
		let (dht_fetched, unfetched, dht_fetch_duration) = match self.network_mode {
			NetworkMode::RPCOnly => (vec![], positions.to_vec(), Duration::from_secs(0)),
			_ => {
				self.fetch_verified_from_dht(block_number, dimensions, commitments, positions)
					.await?
			},
		};

		// Skip RPC retrieval in P2P-only mode
		if self.network_mode == NetworkMode::P2POnly {
			let stats =
				FetchStats::new(positions.len(), dht_fetched.len(), dht_fetch_duration, None);
			return Ok((dht_fetched, unfetched, stats));
		};

		let (rpc_fetched, unfetched, rpc_fetch_duration) = self
			.fetch_verified_from_rpc(
				block_number,
				block_hash,
				dimensions,
				commitments,
				&unfetched,
			)
			.await?;

		// If p2p_client is available and not in RPC-only mode, try to insert the cells into DHT
		if self.network_mode == NetworkMode::Both && self.insert_into_dht {
			let client_guard = self.p2p_client.lock().await;
			if let Some(p2p_client) = client_guard.as_ref() {
				if let Err(error) = p2p_client
					.insert_cells_into_dht(block_number, rpc_fetched.clone())
					.await
				{
					debug!("Error inserting cells into DHT: {error}");
				}
			}
		}

		let stats = FetchStats::new(
			positions.len(),
			dht_fetched.len(),
			dht_fetch_duration,
			Some((rpc_fetched.len(), rpc_fetch_duration)),
		);

		let mut fetched = vec![];
		fetched.extend(dht_fetched);
		fetched.extend(rpc_fetched);

		Ok((fetched, unfetched, stats))
	}
}
pub fn new(
	p2p_client: Arc<Mutex<Option<p2p::Client>>>,
	rpc_client: rpc::Client<impl Database + Sync>,
	pp: Arc<ArkPublicParams>,
	network_mode: NetworkMode,
	insert_into_dht: bool,
) -> impl Client {
	DHTWithRPCFallbackClient {
		p2p_client,
		rpc_client,
		pp,
		network_mode,
		insert_into_dht,
	}
}

struct RPCClient<T: Database> {
	client: rpc::Client<T>,
	pp: Arc<ArkPublicParams>,
}

impl<T: Database> RPCClient<T> {
	async fn fetch_verified_from_rpc(
		&self,
		block_number: u32,
		block_hash: H256,
		dimensions: Dimensions,
		commitments: &Commitments,
		positions: &[Position],
	) -> Result<(Vec<Cell>, Vec<Position>, Duration)> {
		let begin = Instant::now();

		let mut fetched: Vec<Cell> = {
			#[cfg(not(feature = "multiproof"))]
			{
				self.client
					.request_kate_proof(block_hash, positions)
					.await?
					.into_iter()
					.map(Cell::SingleCell)
					.collect()
			}

			#[cfg(feature = "multiproof")]
			{
				self.client
					.request_kate_multi_proof(block_hash, positions)
					.await?
					.into_iter()
					.map(Cell::MultiProofCell)
					.collect()
			}
		};

		let fetch_elapsed = begin.elapsed();
		let (verified, unverified) = proof::verify(
			block_number,
			dimensions,
			&fetched,
			commitments,
			self.pp.clone(),
		)
		.await
		.context("Failed to verify fetched cells")?;

		info!(
			block_number,
			cells_total = positions.len(),
			cells_fetched = fetched.len(),
			cells_verified = verified.len(),
			fetch_elapsed = ?fetch_elapsed,
			proof_verification_elapsed = ?(begin.elapsed() - fetch_elapsed),
			"Cells fetched from RPC"
		);

		fetched.retain(|cell| verified.contains(&cell.position()));
		Ok((fetched, unverified, fetch_elapsed))
	}
}

#[async_trait]
impl<T: Database + Sync> Client for RPCClient<T> {
	async fn fetch_verified(
		&self,
		block_number: u32,
		block_hash: H256,
		dimensions: Dimensions,
		commitments: &Commitments,
		positions: &[Position],
	) -> Result<(Vec<Cell>, Vec<Position>, FetchStats)> {
		let (rpc_fetched, unfetched, rpc_fetch_duration) = self
			.fetch_verified_from_rpc(block_number, block_hash, dimensions, commitments, positions)
			.await?;

		let stats = FetchStats::new(
			positions.len(),
			0,
			Duration::from_secs(0),
			Some((rpc_fetched.len(), rpc_fetch_duration)),
		);

		let mut fetched = vec![];
		fetched.extend(rpc_fetched);

		Ok((fetched, unfetched, stats))
	}
}

pub fn new_rpc(client: rpc::Client<impl Database + Sync>, pp: Arc<ArkPublicParams>) -> impl Client {
	RPCClient { client, pp }
}

#[derive(Clone, ValueEnum, Display)]
#[strum(serialize_all = "kebab-case")]
pub enum Network {
	Local,
	Hex,
	Turing,
	Mainnet,
}

impl Network {
	pub fn bootstrap_peer_id(&self) -> PeerId {
		let peer_id = match self {
			Network::Local => "12D3KooWStAKPADXqJ7cngPYXd2mSANpdgh1xQ34aouufHA2xShz",
			Network::Hex => "12D3KooWBMwfo5qyoLQDRat86kFcGAiJ2yxKM63rXHMw2rDuNZMA",
			#[cfg(feature = "multiproof")]
			Network::Turing => "12D3KooWGsdrGGv7BJjjuRTdDS2yZmSmqL5s3YRQREV6vz9cjrbx",
			#[cfg(not(feature = "multiproof"))]
			Network::Turing => "12D3KooWBkLsNGaD3SpMaRWtAmWVuiZg1afdNSPbtJ8M8r9ArGRT",
			#[cfg(feature = "multiproof")]
			Network::Mainnet => "12D3KooWv4QrjYBIYGkzyz8LdmTi5eXJIukKDShVeAwaAPtWCP8x",
			#[cfg(not(feature = "multiproof"))]
			Network::Mainnet => "12D3KooW9x9qnoXhkHAjdNFu92kMvBRSiFBMAoC5NnifgzXjsuiM",
		};
		PeerId::from_str(peer_id).expect("unable to parse default bootstrap peerID")
	}

	pub fn bootstrap_multiaddr(&self, listeners: &p2p::configuration::Listeners) -> Multiaddr {
		let multiaddr = match listeners {
			Listeners::Tcp | Listeners::Ws => match self {
				Network::Local => "/ip4/127.0.0.1/tcp/39000",
				Network::Hex => "/dns/bootnode.1.lightclient.hex.avail.so/tcp/37000",
				#[cfg(feature = "multiproof")]
				Network::Turing => "/dns/bootnode.1.lightclient.turing-mmp.avail.so/tcp/37000",
				#[cfg(not(feature = "multiproof"))]
				Network::Turing => "/dns/bootnode.1.lightclient.turing.avail.so/tcp/37000",
				#[cfg(feature = "multiproof")]
				Network::Mainnet => "/dns/bootnode.1.lightclient.mainnet-mmp.avail.so/tcp/37000",
				#[cfg(not(feature = "multiproof"))]
				Network::Mainnet => "/dns/bootnode.1.lightclient.mainnet.avail.so/tcp/37000",
			},
			Listeners::Quic | Listeners::QuicTcp => match self {
				Network::Local => "/ip4/127.0.0.1/udp/39000/quic-v1",
				Network::Hex => "/dns/bootnode.1.lightclient.hex.avail.so/udp/37000/quic-v1",
				#[cfg(feature = "multiproof")]
				Network::Turing => "/dns/bootnode.1.lightclient.turing-mmp.avail.so/udp/37000/quic-v1",
				#[cfg(not(feature = "multiproof"))]
				Network::Turing => "/dns/bootnode.1.lightclient.turing.avail.so/udp/37000/quic-v1",
				#[cfg(feature = "multiproof")]
				Network::Mainnet => "/dns/bootnode.1.lightclient.mainnet-mmp.avail.so/udp/37000/quic-v1",
				#[cfg(not(feature = "multiproof"))]
				Network::Mainnet => "/dns/bootnode.1.lightclient.mainnet.avail.so/udp/37000/quic-v1",
			},
		};

		Multiaddr::from_str(multiaddr).expect("unable to parse default bootstrap multi-address")
	}

	pub fn full_node_ws(&self) -> Vec<String> {
		match self {
			Network::Local => vec!["ws://127.0.0.1:9944".to_string()],
			Network::Hex => vec!["wss://rpc-hex-devnet.avail.tools/ws".to_string()],
			Network::Turing => vec!["wss://turing-rpc.avail.so/ws".to_string()],
			Network::Mainnet => vec![
				"wss://mainnet.avail-rpc.com/".to_string(),
				"wss://avail-mainnet.public.blastapi.io/".to_string(),
				"wss://mainnet-rpc.avail.so/ws".to_string(),
			],
		}
	}

	pub fn ot_collector_endpoint(&self) -> &str {
		match self {
			Network::Local => "http://127.0.0.1:4317",
			Network::Hex => "http://otel.lightclient.hex.avail.so:4317",
			Network::Turing => "http://otel.lightclient.turing.avail.so:4317",
			Network::Mainnet => "http://otel.lightclient.mainnet.avail.so:4317",
		}
	}

	pub fn genesis_hash(&self) -> &str {
		match self {
			Network::Local => "DEV",
			Network::Hex => "9d5ea6a5d7631e13028b684a1a0078e3970caa78bd677eaecaf2160304f174fb",
			Network::Turing => "d3d2f3a3495dc597434a99d7d449ebad6616db45e4e4f178f31cc6fa14378b70",
			Network::Mainnet => "b91746b45e0346cc2f815a520b9c6cb4d5c0902af848db0a80f85932d2e8276a",
		}
	}

	pub fn name(genesis_hash: &str) -> String {
		let network = match genesis_hash {
			"b91746b45e0346cc2f815a520b9c6cb4d5c0902af848db0a80f85932d2e8276a" => {
				Network::Mainnet.to_string()
			},
			"9d5ea6a5d7631e13028b684a1a0078e3970caa78bd677eaecaf2160304f174fb" => {
				Network::Hex.to_string()
			},
			"d3d2f3a3495dc597434a99d7d449ebad6616db45e4e4f178f31cc6fa14378b70" => {
				Network::Turing.to_string()
			},
			"DEV" => Network::Local.to_string(),
			_ => "other".to_string(),
		};

		let prefix = &genesis_hash[..std::cmp::min(6, genesis_hash.len())];
		format!("{network}:{prefix}")
	}
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, ValueEnum)]
pub enum AutoNatMode {
	Disabled,
	Enabled,
}
