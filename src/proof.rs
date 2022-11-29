//! Parallelized proof verification

use std::sync::mpsc::channel;

use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use kate_recovery::{data::Cell, matrix::Dimensions, proof};
use tracing::{error, trace};

/// Verifies proofs for given block, cells and commitments
pub fn verify(
	block_num: u32,
	dimensions: &Dimensions,
	cells: &[Cell],
	commitments: &[[u8; 48]],
	public_parameters: &PublicParameters,
) -> usize {
	let cpus = num_cpus::get();
	let pool = threadpool::ThreadPool::new(cpus);
	let (tx, rx) = channel::<bool>();

	for cell in cells {
		let tx = tx.clone();
		let row = cell.position.row;
		let col = cell.position.col;
		let commitment = commitments[row as usize];

		let cell = cell.clone();
		let dimensions = dimensions.clone();
		let public_parameters = public_parameters.clone();

		pool.execute(move || {
			let is_verified =
				match proof::verify(&public_parameters, &dimensions, &commitment, &cell) {
					Ok(is_verified) => {
						trace!(block_num, "Cell ({row}, {col}) verified: {is_verified}");
						is_verified
					},
					Err(error) => {
						error!(block_num, "Verify failed for cell ({row}, {col}): {error}");
						false
					},
				};
			if let Err(error) = tx.send(is_verified) {
				error!("Failed to send proof verified message: {error}");
			};
		});
	}

	rx.iter().take(cells.len()).filter(|&v| v).count()
}
