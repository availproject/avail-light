use anyhow::{Context, Result};
use async_trait::async_trait;
use dusk_plonk::prelude::PublicParameters;
use kate_recovery::{
	config,
	data::Cell,
	matrix::{Dimensions, Position},
};
use mockall::automock;
use sp_core::H256;
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;
use tracing::info;

use crate::proof;

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
		commitments: &[[u8; config::COMMITMENT_SIZE]],
		positions: &[Position],
	) -> Result<(Vec<Cell>, Vec<Position>, FetchStats)>;
}

pub struct FetchStats {
	pub dht_fetched: f64,
	pub dht_fetched_percentage: f64,
	pub dht_fetch_duration: f64,
	pub rpc_fetched: f64,
	pub dht_put_success_rate: Option<f64>,
	pub dht_put_duration: Option<f64>,
}

impl FetchStats {
	pub fn new(
		total: usize,
		dht_fetched: usize,
		dht_fetch_duration: Duration,
		rpc_fetched: usize,
		dht_put_stats: Option<(f64, Duration)>,
	) -> Self {
		FetchStats {
			dht_fetched: dht_fetched as f64,
			dht_fetched_percentage: dht_fetched as f64 / total as f64,
			dht_fetch_duration: dht_fetch_duration.as_secs_f64(),
			rpc_fetched: rpc_fetched as f64,
			dht_put_success_rate: dht_put_stats.map(|(rate, _)| rate),
			dht_put_duration: dht_put_stats.map(|(_, duration)| duration.as_secs_f64()),
		}
	}
}

struct DHTWithRPCFallbackClient {
	p2p_client: p2p::Client,
	rpc_client: rpc::Client,
	pp: Arc<PublicParameters>,
	disable_rpc: bool,
}

type Commitments = [[u8; config::COMMITMENT_SIZE]];

impl DHTWithRPCFallbackClient {
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
	) -> Result<(Vec<Cell>, Vec<Position>)> {
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
		Ok((fetched, unverified))
	}
}

#[async_trait]
impl Client for DHTWithRPCFallbackClient {
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
			let stats = FetchStats::new(
				positions.len(),
				dht_fetched.len(),
				dht_fetch_duration,
				0,
				None,
			);
			return Ok((dht_fetched, unfetched, stats));
		};

		let (rpc_fetched, unfetched) = self
			.fetch_verified_from_rpc(
				block_number,
				block_hash,
				dimensions,
				commitments,
				&unfetched,
			)
			.await?;

		let begin = Instant::now();

		let dht_put_success_rate = self
			.p2p_client
			.insert_cells_into_dht(block_number, rpc_fetched.clone())
			.await;

		info!(
			block_number,
			"DHT PUT operation success rate: {dht_put_success_rate}"
		);

		let stats = FetchStats::new(
			positions.len(),
			dht_fetched.len(),
			dht_fetch_duration,
			rpc_fetched.len(),
			Some((dht_put_success_rate as f64, begin.elapsed())),
		);

		let mut fetched = vec![];
		fetched.extend(dht_fetched);
		fetched.extend(rpc_fetched);

		Ok((fetched, unfetched, stats))
	}
}

pub fn new(
	p2p_client: p2p::Client,
	rpc_client: rpc::Client,
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
