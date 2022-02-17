extern crate threadpool;

use crate::types::Cell;
use dusk_bytes::Serializable;
use dusk_plonk::{
    bls12_381::G1Affine,
    commitment_scheme::kzg10::{commitment::Commitment, proof::Proof, *},
    fft::EvaluationDomain,
};
use merlin::Transcript;
use std::{
    convert::TryInto,
    sync::{mpsc::channel, Arc},
};

pub mod testnet {
    use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
    use rand::SeedableRng;
    use rand_chacha::ChaChaRng;

    pub fn public_params(max_degree: usize) -> PublicParameters {
        let mut rng = ChaChaRng::seed_from_u64(42);
        PublicParameters::setup(max_degree, &mut rng).unwrap()
    }
}

// code for light client to verify incoming kate proofs
// args - now - column number, response (witness + evaluation_point = 48 + 32 bytes), commitment (as bytes)
// args - in future - multiple sets of these
fn kc_verify_proof(
    col_num: u16,
    response: &[u8],
    commitment: &[u8],
    total_rows: usize,
    total_cols: usize,
) -> bool {
    // let total_rows = 128;
    let _extended_total_rows = total_rows * 2;
    // let total_cols = 256;

    let public_params = testnet::public_params(256);
    let raw_pp = public_params.to_raw_var_bytes();
    let hash_pp = hex::encode(sp_core::blake2_128(&raw_pp));
    let hex_pp = hex::encode(raw_pp);
    log::info!("Public params ({}): hash: {}", hex_pp.len(), hash_pp);

    let (_, verifier_key) = public_params.trim(total_cols).unwrap();

    let row_eval_domain = EvaluationDomain::new(total_cols).unwrap();
    let mut row_dom_x_pts = Vec::with_capacity(row_eval_domain.size());
    row_dom_x_pts.extend(row_eval_domain.elements());

    let (witness, eval) = response.split_at(48);

    // log::info!("{:?} {:?}", witness.len(), eval.len());

    let commitment_point = G1Affine::from_bytes(
        commitment
            .try_into()
            .expect("commitment slice with incorrect length"),
    )
    .expect("Invalid commitment point");
    let eval_point = dusk_plonk::prelude::BlsScalar::from_bytes(
        eval.try_into()
            .expect("evaluation point slice with incorrect length"),
    )
    .unwrap();
    let witness_point = G1Affine::from_bytes(
        witness
            .try_into()
            .expect("witness slice with incorrect length"),
    )
    .expect("Invalid witness point");

    let proof = Proof {
        commitment_to_witness: Commitment::from(witness_point),
        evaluated_point: eval_point,
        commitment_to_polynomial: Commitment::from(commitment_point),
    };

    let point = row_dom_x_pts[col_num as usize];
    let verification = verifier_key.batch_check(&[point], &[proof], &mut Transcript::new(b""));
    if let Err(verification_err) = &verification {
        log::info!("Verification error: {:?}", verification_err);
    }

    verification.is_ok()
}

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
    let status = kc_verify_proof(col, proof, commitment, total_rows, total_cols);
    if status {
        log::info!("Verified cell ({}, {}) of block {}", row, col, block_num);
    } else {
        log::info!("Failed for cell ({}, {}) of block {}", row, col, block_num);
    }

    status
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
    let commitment = Arc::new(commitment.clone());

    for cell in cells {
        let _row = cell.row;
        let _col = cell.col;
        let tx = tx.clone();
        let commitment = commitment.clone();

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
