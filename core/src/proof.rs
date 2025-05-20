use avail_rust::kate_recovery::{
<<<<<<< HEAD
<<<<<<< HEAD
<<<<<<< HEAD
	commons::ArkPublicParams,
	data::{Cell, SingleCell},
=======
	data::CellVariant,
>>>>>>> b2cc124a (multiproofs: Part II)
=======
	data::CellType,
>>>>>>> 47071951 (rename cell variant)
=======
	data::Cell,
>>>>>>> 23e1a765 (rename CellType)
	matrix::{Dimensions, Position},
	proof::{verify_v2, Error},
};
<<<<<<< HEAD
<<<<<<< HEAD

use color_eyre::eyre;
=======
#[cfg(feature = "multiproof")]
use avail_rust::{
	kate_recovery::data::GCellBlock,
	primitives::kate::{GMultiProof, GProof},
	rpc::kate::{generate_pmp, verify_multi_proof},
	U256,
};
use color_eyre::eyre;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
<<<<<<< HEAD
#[cfg(not(feature = "multiproof"))]
>>>>>>> b2cc124a (multiproofs: Part II)
use futures::future::join_all;
#[cfg(not(feature = "multiproof"))]
use itertools::{Either, Itertools};
=======
use color_eyre::eyre;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
>>>>>>> 8bd2c48f (optimize pmp init)
=======
use std::sync::Arc;
>>>>>>> 73dd1331 (update proof for fat client)
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
	tracing::info,
	tracing::warn,
};

<<<<<<< HEAD
<<<<<<< HEAD
#[cfg(not(feature = "multiproof"))]
use crate::utils::spawn_in_span;

<<<<<<< HEAD
use crate::utils::spawn_in_span;

=======
=======
>>>>>>> 23e1a765 (rename CellType)
=======
#[cfg(not(feature = "multiproof"))]
use crate::utils::spawn_in_span;

>>>>>>> 73dd1331 (update proof for fat client)
mod core;

#[cfg(not(feature = "multiproof"))]
<<<<<<< HEAD
>>>>>>> b2cc124a (multiproofs: Part II)
=======
#[allow(deprecated)]
>>>>>>> 2ecc1038 (merge changes from main)
async fn verify_proof(
	public_parameters: Arc<ArkPublicParams>,
	dimensions: Dimensions,
	commitment: [u8; 48],
	cell: SingleCell,
<<<<<<< HEAD
) -> Result<(Position, bool), Error> {
	verify_v2(&public_parameters, dimensions, &commitment, &cell)
=======
) -> Result<(Position, bool), core::Error> {
	core::verify(&public_parameters, dimensions, &commitment, &cell)
>>>>>>> 23e1a765 (rename CellType)
		.map(|verified| (cell.position, verified))
}

/// Verifies proofs for given block, cells and commitments
#[cfg(not(feature = "multiproof"))]
pub async fn verify(
	block_num: u32,
	dimensions: Dimensions,
	cells: &[Cell],
	commitments: &[[u8; 48]],
	public_parameters: Arc<ArkPublicParams>,
) -> eyre::Result<(Vec<Position>, Vec<Position>)> {
	if cells.is_empty() {
		return Ok((Vec::new(), Vec::new()));
<<<<<<< HEAD
<<<<<<< HEAD
	}

=======
	};
>>>>>>> b2cc124a (multiproofs: Part II)
=======
	}

>>>>>>> 23e1a765 (rename CellType)
	let start_time = Instant::now();

	let tasks = cells
		.iter()
<<<<<<< HEAD
<<<<<<< HEAD
		.filter_map(|cell_type| {
			SingleCell::try_from(cell_type.clone()).ok().map(|cell| {
=======
		.filter_map(|cell_variant| {
			Cell::try_from(cell_variant.clone()).ok().map(|cell| {
>>>>>>> b2cc124a (multiproofs: Part II)
=======
		.filter_map(|cell_type| {
<<<<<<< HEAD
			Cell::try_from(cell_type.clone()).ok().map(|cell| {
>>>>>>> 8bd2c48f (optimize pmp init)
=======
			SingleCell::try_from(cell_type.clone()).ok().map(|cell| {
>>>>>>> 23e1a765 (rename CellType)
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

	let results: Vec<(Position, bool)> = join_results
		.into_iter()
		.map(|r| r.map_err(|e| eyre::eyre!("{:?}", e)))
		.collect::<Result<_, _>>()?;

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
