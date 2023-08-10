//! Parallelized proof verification

use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use itertools::{Either, Itertools};
use kate_recovery::{
	data::Cell,
	matrix::{Dimensions, Position},
	proof,
};
use std::sync::{mpsc::channel, Arc};
use tracing::error;

/// Verifies proofs for given block, cells and commitments
pub fn verify(
	block_num: u32,
	dimensions: Dimensions,
	cells: &[Cell],
	commitments: &[[u8; 48]],
	public_parameters: Arc<PublicParameters>,
) -> Result<(Vec<Position>, Vec<Position>), proof::Error> {
	let cpus = num_cpus::get();
	let pool = threadpool::ThreadPool::new(cpus);
	let (tx, rx) = channel::<(Position, Result<bool, proof::Error>)>();

	for cell in cells {
		let commitment = commitments[cell.position.row as usize];

		let tx = tx.clone();
		let cell = cell.clone();
		let public_parameters = public_parameters.clone();

		pool.execute(move || {
			let result = proof::verify(&public_parameters, dimensions, &commitment, &cell);
			if let Err(error) = tx.clone().send((cell.position, result)) {
				error!(block_num, "Failed to send proof verified message: {error}");
			}
		});
	}

	let (verified, unverified): (Vec<_>, Vec<_>) = rx
		.iter()
		.take(cells.len())
		.map(|(position, result)| result.map(|is_verified| (position, is_verified)))
		.collect::<Result<Vec<(Position, bool)>, _>>()?
		.into_iter()
		.partition_map(|(position, is_verified)| match is_verified {
			true => Either::Left(position),
			false => Either::Right(position),
		});

	Ok((verified, unverified))
}
