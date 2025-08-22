use kate_recovery::{
	commons::ArkPublicParams,
	data::Cell,
	matrix::{Dimensions, Position},
};

use color_eyre::eyre;
#[cfg(feature = "multiproof")]
use kate::pmp::{ark_bls12_381::Bls12_381, method1::M1NoPrecomp, msm::blst::BlstMSMEngine};
#[cfg(feature = "multiproof")]
use kate_recovery::proof::verify_multi_proof;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::Instant;
use tracing::debug;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;
#[cfg(feature = "multiproof")]
use {
	avail_rust::U256,
	tokio::sync::OnceCell,
	tracing::{info, warn},
};

#[cfg(feature = "multiproof")]
use {
	avail_core::AppExtrinsic,
	kate::{
		couscous::multiproof_params,
		Seed,
	},
	kate_recovery::data::GCellBlock,
};

// Define additional types that may be needed
#[cfg(feature = "multiproof")]
pub type BlockLength = u32; // Simplified, adjust based on actual type



#[cfg(not(feature = "multiproof"))]
use {
	futures::future::join_all,
	itertools::{Either, Itertools},
	kate_recovery::{
		data::SingleCell,
		proof::{verify_v2, Error},
	},
};

#[cfg(not(feature = "multiproof"))]
use crate::utils::spawn_in_span;

#[cfg(not(feature = "multiproof"))]
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
	cells: &[Cell],
	commitments: &[[u8; 48]],
	public_parameters: Arc<ArkPublicParams>,
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
	_public_parameters: Arc<ArkPublicParams>,
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
	let is_verified = verify_multi_proof(pmp, &proof_pairs, &commitments, cols.into())
		.await
		.map_err(|e| eyre::eyre!("{:?}", e))?;

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
		let pmp = multiproof_params();
		info!("PMP initialized successfully");
		pmp
	})
	.await
}

#[cfg(feature = "multiproof")]
pub async fn get_or_init_srs() -> &'static M1NoPrecomp<Bls12_381, BlstMSMEngine> {
	SRS.get_or_init(|| async {
		let srs = multiproof_params();
		info!("SRS initialized successfully");
		srs
	})
	.await
}

#[cfg(feature = "multiproof")]
pub static PMP: OnceCell<M1NoPrecomp<Bls12_381, BlstMSMEngine>> = OnceCell::const_new();

#[cfg(feature = "multiproof")]
pub static SRS: OnceCell<M1NoPrecomp<Bls12_381, BlstMSMEngine>> = OnceCell::const_new();

/// Generates multiproofs for given extrinsics and cell positions
/// 
#[cfg(feature = "multiproof")]
pub async fn multiproof(
	_extrinsics: Vec<AppExtrinsic>,
	_block_len: BlockLength,
	_seed: Seed,
	_cells: Vec<(u32, u32)>,
) -> eyre::Result<Vec<(Vec<u8>, GCellBlock)>> {
	// TODO: Implement proper multiproof generation when std feature is available
	// This requires EvaluationGrid and related types from kate::gridgen::core
	Err(eyre::eyre!(
		"Multiproof generation requires std feature for kate library. \
		Enable std feature or implement with available no-std compatible types."
	))
}
