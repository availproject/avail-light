use avail_rust::kate_recovery::{
	commons::ArkPublicParams,
	data::{Cell, SingleCell},
	matrix::{Dimensions, Position},
};
use color_eyre::eyre;
use futures::future::join_all;
use itertools::{Either, Itertools};
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::Instant;
use tracing::debug;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

mod core;

use crate::utils::spawn_in_span;

async fn verify_proof(
	public_parameters: Arc<ArkPublicParams>,
	dimensions: Dimensions,
	commitment: [u8; 48],
	cell: SingleCell,
) -> Result<(Position, bool), core::Error> {
	core::verify_v2(&public_parameters, dimensions, &commitment, &cell)
		.map(|verified| (cell.position, verified))
}

/// Verifies proofs for given block, cells and commitments
pub async fn verify(
	block_num: u32,
	dimensions: Dimensions,
	cells: &[Cell],
	commitments: &[[u8; 48]],
	public_parameters: Arc<ArkPublicParams>,
) -> eyre::Result<(Vec<Position>, Vec<Position>)> {
	if cells.is_empty() {
		return Ok((Vec::new(), Vec::new()));
	}

	let start_time = Instant::now();

	let tasks = cells
		.iter()
		.filter_map(|cell_type| {
			SingleCell::try_from(cell_type.clone()).ok().map(|cell| {
				spawn_in_span(verify_proof(
					public_parameters.clone(),
					dimensions,
					commitments[cell.position.row as usize],
					cell,
				))
			})
		})
		.collect::<Vec<_>>();

	let join_results: Vec<_> = join_all(tasks)
		.await
		.into_iter()
		.collect::<Result<_, _>>()?;

	let results: Vec<(Position, bool)> = join_results.into_iter().collect::<Result<_, _>>()?;

	debug!(block_num, duration = ?start_time.elapsed(), "Proof verification completed");

	Ok(results
		.into_iter()
		.partition_map(|(position, is_verified)| {
			if is_verified {
				Either::Left(position)
			} else {
				Either::Right(position)
			}
		}))
}
