use std::sync::Arc;

use avail_rust::kate_recovery::{
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
#[cfg(not(feature = "multiproof"))]
>>>>>>> b2cc124a (multiproofs: Part II)
use futures::future::join_all;
#[cfg(not(feature = "multiproof"))]
use itertools::{Either, Itertools};
=======
use color_eyre::eyre;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
>>>>>>> 8bd2c48f (optimize pmp init)
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::Instant;
use tracing::debug;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;
#[cfg(not(feature = "multiproof"))]
use {
	avail_rust::kate_recovery::data::Cell,
	futures::future::join_all,
	itertools::{Either, Itertools},
};
#[cfg(feature = "multiproof")]
use {
	avail_rust::{
		primitives::kate::GProof,
		rpc::kate::{generate_pmp, verify_multi_proof},
		Bls12_381, BlstMSMEngine, M1NoPrecomp, U256,
	},
	tokio::{sync::OnceCell, task},
	tracing::{info, warn},
};

#[cfg(not(feature = "multiproof"))]
use crate::utils::spawn_in_span;

<<<<<<< HEAD
use crate::utils::spawn_in_span;

=======
mod core;

#[cfg(not(feature = "multiproof"))]
>>>>>>> b2cc124a (multiproofs: Part II)
async fn verify_proof(
	public_parameters: Arc<ArkPublicParams>,
	dimensions: Dimensions,
	commitment: [u8; 48],
	cell: SingleCell,
) -> Result<(Position, bool), Error> {
	verify_v2(&public_parameters, dimensions, &commitment, &cell)
		.map(|verified| (cell.position, verified))
}

/// Verifies proofs for given block, cells and commitments
#[cfg(not(feature = "multiproof"))]
pub async fn verify(
	block_num: u32,
	dimensions: Dimensions,
	cells: &[CellType],
	commitments: &[[u8; 48]],
	public_parameters: Arc<ArkPublicParams>,
) -> eyre::Result<(Vec<Position>, Vec<Position>)> {
	if cells.is_empty() {
		return Ok((Vec::new(), Vec::new()));
<<<<<<< HEAD
	}

=======
	};
>>>>>>> b2cc124a (multiproofs: Part II)
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
			Cell::try_from(cell_type.clone()).ok().map(|cell| {
>>>>>>> 8bd2c48f (optimize pmp init)
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
	cells: &[CellType],
	commitments: &[[u8; 48]],
	_public_parameters: Arc<PublicParameters>,
) -> eyre::Result<(Vec<Position>, Vec<Position>)> {
	if cells.is_empty() {
		return Ok((vec![], vec![]));
	}

	let start_time = Instant::now();
	let pmp = get_or_init_pmp().await;
	let cols = dimensions.cols().get();

	let mut proof_pairs = Vec::with_capacity(cells.len());
	let mut positions = Vec::with_capacity(cells.len());

	for cell in cells.iter().cloned() {
		if let CellType::MCell(mcell) = cell {
			let scalars = mcell
				.scalars
				.into_iter()
				.map(|limbs| U256(limbs))
				.collect::<Vec<_>>();

			let gproof = GProof(mcell.proof);
			let gcell_block = mcell.gcell_block;

			proof_pairs.push(((scalars, gproof), gcell_block));
			positions.push(mcell.position);
		}
	}

	let commitments = commitments.iter().flatten().copied().collect::<Vec<u8>>();
	let is_verified =
		verify_multi_proof(pmp.clone(), proof_pairs, commitments, cols.into()).await?;

	debug!(
		block_num,
		verified = is_verified,
		duration = ?start_time.elapsed(),
		"Multiproof verification completed",
	);

	let (verified, unverified) = positions.into_iter().partition(|_| is_verified);

	Ok((verified, unverified))
}

#[cfg(feature = "multiproof")]
pub fn spawn_pmp_initializer() {
	task::spawn(async {
		let pmp = generate_pmp().await;
		if let Err(_) = PMP.set(pmp) {
			warn!("PMP was already initialized");
		} else {
			info!("PMP initialized successfully");
		}
	});
}

#[cfg(feature = "multiproof")]
pub async fn get_or_init_pmp() -> &'static M1NoPrecomp<Bls12_381, BlstMSMEngine> {
	PMP.get_or_init(generate_pmp).await
}

#[cfg(feature = "multiproof")]
pub static PMP: OnceCell<M1NoPrecomp<Bls12_381, BlstMSMEngine>> = OnceCell::const_new();
