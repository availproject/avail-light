//! Parallelized proof verification

use color_eyre::eyre;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use itertools::{Either, Itertools};
use kate_recovery::{
	data::Cell,
	matrix::{Dimensions, Position},
	proof,
};
use std::sync::Arc;
use tokio::{task::JoinSet, time::Instant};
use tracing::debug;

async fn verify_proof(
	public_parameters: Arc<PublicParameters>,
	dimensions: Dimensions,
	commitment: [u8; 48],
	cell: Cell,
) -> Result<(Position, bool), proof::Error> {
	proof::verify(&public_parameters, dimensions, &commitment, &cell)
		.map(|verified| (cell.position, verified))
}

/// Verifies proofs for given block, cells and commitments
pub async fn verify(
	block_num: u32,
	dimensions: Dimensions,
	cells: &[Cell],
	commitments: &[[u8; 48]],
	public_parameters: Arc<PublicParameters>,
) -> eyre::Result<(Vec<Position>, Vec<Position>)> {
	if cells.is_empty() {
		return Ok((Vec::new(), Vec::new()));
	};

	let start_time = Instant::now();

	let mut tasks = JoinSet::new();

	for cell in cells {
		tasks.spawn(verify_proof(
			public_parameters.clone(),
			dimensions,
			commitments[cell.position.row as usize],
			cell.clone(),
		));
	}

	let mut results = Vec::with_capacity(cells.len());
	while let Some(result) = tasks.join_next().await {
		results.push(result??)
	}

	debug!(block_num, duration = ?start_time.elapsed(), "Proof verification completed");

	Ok(results
		.into_iter()
		.partition_map(|(position, is_verified)| match is_verified {
			true => Either::Left(position),
			false => Either::Right(position),
		}))
}
