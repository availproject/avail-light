//! Parallelized proof verification

use std::sync::{mpsc::channel, Arc};

use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use kate_recovery::com::Cell;
use tracing::{error, trace};

// Just a wrapper function, to be used when spawning threads for verifying proofs
// for a certain block
fn kc_verify_proof_wrapper(
	block_num: u32,
	row: u16,
	col: u16,
	total_rows: usize,
	total_cols: usize,
	proof: &[u8],
	commitment: &[u8],
	pp: PublicParameters,
) -> bool {
	match kate_proof::kc_verify_proof(col as u32, proof, commitment, total_rows, total_cols, &pp) {
		Ok(ver) => {
			trace!("Verified cell ({row}, {col}) of block {block_num}");
			ver
		},
		Err(error) => {
			error!("Verify failed for cell ({row}, {col}) of block {block_num}: {error}");
			false
		},
	}
}

/// Verifies proofs for given block, cells and commitments
pub fn verify_proof(
	block_num: u32,
	total_rows: u16,
	total_cols: u16,
	cells: &[Cell],
	commitment: Vec<u8>,
	pp: PublicParameters,
) -> usize {
	let cpus = num_cpus::get();
	let pool = threadpool::ThreadPool::new(cpus);
	let (tx, rx) = channel::<bool>();
	let jobs = cells.len();
	let commitment = Arc::new(commitment);

	for cell in cells.iter().cloned() {
		let row = cell.position.row;
		let col = cell.position.col;
		let tx = tx.clone();
		let commitment = commitment.clone();
		let params = pp.clone();

		pool.execute(move || {
			if let Err(error) = tx.send(kc_verify_proof_wrapper(
				block_num,
				row,
				col,
				total_rows as usize,
				total_cols as usize,
				&cell.content,
				&commitment[row as usize * 48..(row as usize + 1) * 48],
				params,
			)) {
				error!("Failed to send proof verified message: {error}");
			};
		});
	}

	rx.iter().take(jobs).filter(|&v| v).count()
}
