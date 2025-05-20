use avail_rust::kate_recovery::{
	data::Cell,
	matrix::{Dimensions, Position},
};
use color_eyre::eyre;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::Instant;
use tracing::debug;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;
#[cfg(not(feature = "multiproof"))]
use {
	avail_rust::kate_recovery::data::SingleCell,
	futures::future::join_all,
	itertools::{Either, Itertools},
};
#[cfg(feature = "multiproof")]
use {
	avail_rust::{
		kate_recovery::proof::verify_multi_proof, rpc::kate::generate_pmp, Bls12_381,
		BlstMSMEngine, M1NoPrecomp, U256,
	},
	tokio::sync::OnceCell,
	tracing::{info, warn},
};

#[cfg(not(feature = "multiproof"))]
use crate::utils::spawn_in_span;

mod core;

#[cfg(not(feature = "multiproof"))]
#[allow(deprecated)]
async fn verify_proof(
	public_parameters: Arc<PublicParameters>,
	dimensions: Dimensions,
	commitment: [u8; 48],
	cell: SingleCell,
) -> Result<(Position, bool), core::Error> {
	core::verify(&public_parameters, dimensions, &commitment, &cell)
		.map(|verified| (cell.position, verified))
}

/// Verifies proofs for given block, cells and commitments
#[cfg(not(feature = "multiproof"))]
pub async fn verify(
	block_num: u32,
	dimensions: Dimensions,
	cells: &[Cell],
	commitments: &[[u8; 48]],
	public_parameters: Arc<PublicParameters>,
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

/// Verifies multiproofs for the given block, cells, and commitments.
#[cfg(feature = "multiproof")]
pub async fn verify(
	block_num: u32,
	dimensions: Dimensions,
	cells: &[Cell],
	commitments: &[[u8; 48]],
	_public_parameters: Arc<PublicParameters>,
) -> eyre::Result<(Vec<Position>, Vec<Position>)> {
	if cells.is_empty() {
		return Ok((Vec::new(), Vec::new()));
	}

	let start_time = Instant::now();
	let pmp = get_or_init_pmp().await;
	let cols = dimensions.cols().get();

	let mut proof_pairs = Vec::with_capacity(cells.len());
	let mut positions = Vec::with_capacity(cells.len());

	for cell in cells {
		let mcell = match cell {
			Cell::MultiProofCell(m) => m,
			_ => {
				warn!("incorrect cell type received: {:?}", cell);
				continue;
			},
		};

		let scalars: Vec<[u8; 32]> = mcell
			.scalars
			.iter()
			.map(|&limbs| U256(limbs).to_big_endian())
			.collect();

		proof_pairs.push(((scalars, mcell.proof), mcell.gcell_block.clone()));
		positions.push(mcell.position);
	}

	let commitments = commitments.iter().flatten().copied().collect::<Vec<u8>>();
	let is_verified = verify_multi_proof(&pmp, &proof_pairs, &commitments, cols.into()).await?;

	debug!(
		block_num,
		verified = is_verified,
		duration = ?start_time.elapsed(),
		"Multiproof verification completed"
	);

	let (verified, unverified) = positions.into_iter().partition(|_| is_verified);
	Ok((verified, unverified))
}

#[cfg(feature = "multiproof")]
pub async fn get_or_init_pmp() -> &'static M1NoPrecomp<Bls12_381, BlstMSMEngine> {
	PMP.get_or_init(|| async {
		let pmp = generate_pmp().await;
		info!("PMP initialized successfully");
		pmp
	})
	.await
}

#[cfg(feature = "multiproof")]
pub static PMP: OnceCell<M1NoPrecomp<Bls12_381, BlstMSMEngine>> = OnceCell::const_new();
