//! Parallelized proof verification

use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use itertools::{Either, Itertools};
use kate_recovery::{
	data::Cell,
	matrix::{Dimensions, Position},
	proof,
};
use rayon::prelude::*;
use std::sync::Arc;

/// Verifies proofs for given block, cells and commitments
pub fn verify(
	dimensions: Dimensions,
	cells: &[Cell],
	commitments: &[[u8; 48]],
	public_parameters: Arc<PublicParameters>,
) -> Result<(Vec<Position>, Vec<Position>), proof::Error> {
	let (verified, unverified) = cells
		.into_par_iter()
		.map(|cell| {
			let commitment = &commitments[cell.position.row as usize];
			proof::verify(&public_parameters, dimensions, commitment, cell)
				.map(|is_verified| (cell.position, is_verified))
		})
		.take(cells.len())
		.collect::<Result<Vec<(Position, bool)>, _>>()?
		.into_iter()
		.partition_map(|(position, is_verified)| match is_verified {
			true => Either::Left(position),
			false => Either::Right(position),
		});

	Ok((verified, unverified))
}
