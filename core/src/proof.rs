use std::sync::Arc;

#[cfg(not(feature = "multiproof"))]
use crate::utils::spawn_in_span;
#[cfg(not(feature = "multiproof"))]
use avail_rust::kate_recovery::data::Cell;
use avail_rust::kate_recovery::{
	data::CellVariant,
	matrix::{Dimensions, Position},
};
#[cfg(feature = "multiproof")]
use avail_rust::{
	kate_recovery::data::GCellBlock,
	primitives::kate::{GMultiProof, GProof},
	rpc::kate::{generate_pmp, verify_multi_proof},
	U256,
};
use color_eyre::eyre;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
#[cfg(not(feature = "multiproof"))]
use futures::future::join_all;
#[cfg(not(feature = "multiproof"))]
use itertools::{Either, Itertools};
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::Instant;
use tracing::debug;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

mod core;

#[cfg(not(feature = "multiproof"))]
async fn verify_proof(
	public_parameters: Arc<PublicParameters>,
	dimensions: Dimensions,
	commitment: [u8; 48],
	cell: Cell,
) -> Result<(Position, bool), core::Error> {
	core::verify(&public_parameters, dimensions, &commitment, &cell)
		.map(|verified| (cell.position, verified))
}

/// Verifies proofs for given block, cells and commitments
#[cfg(not(feature = "multiproof"))]
pub async fn verify(
	block_num: u32,
	dimensions: Dimensions,
	cells: &[CellVariant],
	commitments: &[[u8; 48]],
	public_parameters: Arc<PublicParameters>,
) -> eyre::Result<(Vec<Position>, Vec<Position>)> {
	if cells.is_empty() {
		return Ok((Vec::new(), Vec::new()));
	};
	let start_time = Instant::now();
	let tasks = cells
		.iter()
		.filter_map(|cell_variant| {
			Cell::try_from(cell_variant.clone()).ok().map(|cell| {
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
		.partition_map(|(position, is_verified)| match is_verified {
			true => Either::Left(position),
			false => Either::Right(position),
		}))
}

/// Verifies multiproofs for given block, cells and commitments
#[cfg(feature = "multiproof")]
pub async fn verify(
	block_num: u32,
	dimensions: Dimensions,
	cells: &[CellVariant],
	commitments: &[[u8; 48]],
	_public_parameters: Arc<PublicParameters>,
) -> eyre::Result<(Vec<Position>, Vec<Position>)> {
	if cells.is_empty() {
		return Ok((vec![], vec![]));
	}

	let start_time = Instant::now();
	let pmp = generate_pmp().await;
	let cols = dimensions.cols().get();

	let mut proof_pairs: Vec<(GMultiProof, GCellBlock)> = Vec::with_capacity(cells.len());
	let mut positions: Vec<Position> = Vec::with_capacity(cells.len());

	for cell in cells {
		if let CellVariant::MCell(mcell) = cell {
			let scalars = mcell
				.scalars
				.iter()
				.map(|limbs| U256(*limbs))
				.collect::<Vec<U256>>();

			let gproof = GProof(mcell.proof);
			let gcell_block = mcell.gcell_block.clone();

			proof_pairs.push(((scalars, gproof), gcell_block));
			positions.push(mcell.position);
		}
	}

	let flat_commitments = commitments.iter().flatten().copied().collect::<Vec<u8>>();

	let is_verified = verify_multi_proof(pmp, proof_pairs, flat_commitments, cols.into()).await?;

	debug!(
		block_num,
		verified = is_verified,
		duration = ?start_time.elapsed(),
		"Multiproof verification completed"
	);

	let (verified, unverified): (Vec<Position>, Vec<Position>) =
		positions.into_iter().partition(|_| is_verified);

	Ok((verified, unverified))
}
