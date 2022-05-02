extern crate threadpool;

use std::sync::{mpsc::channel, Arc};

use crate::types::Cell;

// Just a wrapper function, to be used when spawning threads for verifying proofs
// for a certain block
fn kc_verify_proof_wrapper(
	block_num: u64,
	row: u16,
	col: u16,
	total_rows: usize,
	total_cols: usize,
	proof: &[u8],
	commitment: &[u8],
) -> bool {
	match kate_proof::kc_verify_proof(col, proof, commitment, total_rows, total_cols) {
		Ok(verification) => {
			log::info!(
				"Public params ({}): hash: {}",
				verification.public_params_len,
				verification.public_params_hash
			);
			match &verification.status {
				Ok(()) => {
					log::info!("Verified cell ({}, {}) of block {}", row, col, block_num);
				},
				Err(verification_err) => {
					log::error!("Verification error: {:?}", verification_err);
					log::error!("Failed for cell ({}, {}) of block {}", row, col, block_num);
				},
			}

			verification.status.is_ok()
		},
		Err(error) => {
			log::error!(
				"Failed for cell ({}, {}) of block {} with error {}",
				row,
				col,
				block_num,
				error
			);
			false
		},
	}
}

pub fn verify_proof(
	block_num: u64,
	total_rows: u16,
	total_cols: u16,
	cells: Vec<Cell>,
	commitment: Vec<u8>,
) -> u32 {
	let cpus = num_cpus::get();
	let pool = threadpool::ThreadPool::new(cpus);
	let (tx, rx) = channel::<bool>();
	let jobs = cells.len();
	let commitment = Arc::new(commitment);

	for cell in cells {
		let _row = cell.row;
		let _col = cell.col;
		let tx = tx.clone();
		let commitment = Arc::clone(&commitment);

		pool.execute(move || {
			tx.send(kc_verify_proof_wrapper(
				block_num,
				_row,
				_col,
				total_rows as usize,
				total_cols as usize,
				&cell.proof,
				&commitment[_row as usize * 48..(_row as usize + 1) * 48],
			))
			.unwrap();
		});
	}

	rx.iter().take(jobs).filter(|&v| v).count() as u32
}
