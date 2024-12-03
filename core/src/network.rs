use async_trait::async_trait;
use avail_rust::{
	avail_core::kate::COMMITMENT_SIZE,
	kate_recovery::{
		data::Cell,
		matrix::{Dimensions, Position},
	},
	H256,
};
use clap::ValueEnum;
use color_eyre::{eyre::WrapErr, Result};
use dusk_plonk::prelude::PublicParameters;
use libp2p::{Multiaddr, PeerId};
use mockall::automock;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
use std::{str::FromStr, sync::Arc};
use strum::Display;
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::Instant;
use tracing::{debug, info};
#[cfg(target_arch = "wasm32")]
use web_time::{Duration, Instant};

use crate::{data::Database, proof};

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
	p2p_client: p2p::Client,
	rpc_client: rpc::Client<T>,
	pp: Arc<PublicParameters>,
	disable_rpc: bool,
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

		let (mut dht_fetched, mut unfetched) = self
			.p2p_client
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

		dht_fetched.retain(|cell| verified.contains(&cell.position));
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

		let mut fetched = self
			.rpc_client
			.request_kate_proof(block_hash, positions)
			.await?;

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

		fetched.retain(|cell| verified.contains(&cell.position));
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
		let (dht_fetched, unfetched, dht_fetch_duration) = self
			.fetch_verified_from_dht(block_number, dimensions, commitments, positions)
			.await?;

		if self.disable_rpc {
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

		if let Err(error) = self
			.p2p_client
			.insert_cells_into_dht(block_number, rpc_fetched.clone())
			.await
		{
			debug!("Error inserting cells into DHT: {error}");
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
	p2p_client: p2p::Client,
	rpc_client: rpc::Client<impl Database + Sync>,
	pp: Arc<PublicParameters>,
	disable_rpc: bool,
) -> impl Client {
	DHTWithRPCFallbackClient {
		p2p_client,
		rpc_client,
		pp,
		disable_rpc,
	}
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
			Network::Turing => "12D3KooWBkLsNGaD3SpMaRWtAmWVuiZg1afdNSPbtJ8M8r9ArGRT",
			Network::Mainnet => "12D3KooW9x9qnoXhkHAjdNFu92kMvBRSiFBMAoC5NnifgzXjsuiM",
		};
		PeerId::from_str(peer_id).expect("unable to parse default bootstrap peerID")
	}

	pub fn bootstrap_multiaddr(&self) -> Multiaddr {
		let multiaddr = match self {
			Network::Local => "/ip4/127.0.0.1/tcp/39000",
			Network::Hex => "/dns/bootnode.1.lightclient.hex.avail.so/tcp/37000",
			Network::Turing => "/dns/bootnode.1.lightclient.turing.avail.so/tcp/37000",
			Network::Mainnet => "/dns/bootnode.1.lightclient.mainnet.avail.so/tcp/37000",
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
		format!("{}:{}", network, prefix)
	}
}
