use color_eyre::eyre;
#[cfg(feature = "multiproof")]
use kate::pmp::{ark_bls12_381::Bls12_381, method1::M1NoPrecomp, msm::blst::BlstMSMEngine};
#[cfg(feature = "multiproof")]
use kate_recovery::proof::verify_multi_proof;
use kate_recovery::{
	commons::ArkPublicParams,
	data::Cell,
	matrix::{Dimensions, Position},
};
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
	avail_core::{AppExtrinsic, BlockLengthColumns, BlockLengthRows},
	core::num::NonZeroU16,
	kate::{couscous::multiproof_params, ArkScalar, Seed},
	kate_recovery::data::GCellBlock,
	rayon::iter::{IntoParallelIterator, ParallelIterator},
};

#[cfg(feature = "multiproof")]
use kate::{
	com::Cell as KateCell,
	gridgen::core::{AsBytes as _, EvaluationGrid as EGrid},
};

#[cfg(feature = "multiproof")]
pub type GMultiProof = (Vec<GRawScalar>, GProof);

#[cfg(feature = "multiproof")]
#[derive(Clone, Debug)]
pub struct GProof(pub [u8; 48]);

#[cfg(feature = "multiproof")]
#[derive(Clone, Debug)]
pub struct GRawScalar(pub [u64; 4]);
// Simplified block length for this implementation
#[cfg(feature = "multiproof")]
#[derive(Clone, Copy)]
pub struct BlockLength {
	pub rows: BlockLengthRows,
	pub cols: BlockLengthColumns,
}

#[cfg(feature = "multiproof")]
impl BlockLength {
	pub fn new(rows: u32, cols: u32) -> Self {
		Self {
			rows: BlockLengthRows(rows),
			cols: BlockLengthColumns(cols),
		}
	}
}

#[cfg(feature = "multiproof")]
const MIN_WIDTH: usize = 4;

#[cfg(not(feature = "multiproof"))]
use {
	futures::future::join_all,
	itertools::{Either, Itertools},
	kate_recovery::{data::SingleCell, proof::verify_v2},
};

#[cfg(not(feature = "multiproof"))]
use crate::utils::spawn_in_span;

#[cfg(not(feature = "multiproof"))]
async fn verify_proof(
	public_parameters: Arc<ArkPublicParams>,
	dimensions: Dimensions,
	commitment: [u8; 48],
	cell: SingleCell,
) -> eyre::Result<(Position, bool)> {
	verify_v2(&public_parameters, dimensions, &commitment, &cell)
		.map(|verified| (cell.position, verified))
		.map_err(|e| eyre::eyre!("Proof verification failed: {:?}", e))
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
pub fn get_or_init_srs() -> &'static M1NoPrecomp<Bls12_381, BlstMSMEngine> {
	SRS.get_or_init(|| {
		let srs = multiproof_params();
		info!("SRS initialized successfully");
		srs
	})
}

#[cfg(feature = "multiproof")]
pub static PMP: OnceCell<M1NoPrecomp<Bls12_381, BlstMSMEngine>> = OnceCell::const_new();

#[cfg(feature = "multiproof")]
static SRS: std::sync::OnceLock<M1NoPrecomp<Bls12_381, BlstMSMEngine>> = std::sync::OnceLock::new();

/// Helper function to convert block length to width/height like in runtime
#[cfg(feature = "multiproof")]
fn to_width_height(block_len: &BlockLength) -> (usize, usize) {
	let width = block_len.cols.0 as usize;
	let height = block_len.rows.0 as usize;
	(width, height)
}

/// Generates multiproofs for given extrinsics and cell positions
#[cfg(feature = "multiproof")]
pub fn multiproof(
	extrinsics: Vec<AppExtrinsic>,
	block_len: BlockLength,
	seed: Seed,
	cells: Vec<(u32, u32)>,
) -> eyre::Result<Vec<(GMultiProof, GCellBlock)>> {
	let srs = SRS.get_or_init(multiproof_params);
	let (max_width, max_height) = to_width_height(&block_len);
	let grid = EGrid::from_extrinsics(extrinsics, MIN_WIDTH, max_width, max_height, seed)
		.map_err(|e| eyre::eyre!("Failed to create grid from extrinsics: {:?}", e))?
		.extend_columns(NonZeroU16::new(2).expect("2>0"))
		.map_err(|_| eyre::eyre!("Column extension failed"))?;

	let poly = grid
		.make_polynomial_grid()
		.map_err(|e| eyre::eyre!("Failed to create polynomial grid: {:?}", e))?;

	let proofs = cells
		.into_par_iter()
		.map(|(row, col)| -> eyre::Result<(GMultiProof, GCellBlock)> {
			let cell = KateCell::new(BlockLengthRows(row), BlockLengthColumns(col));
			let target_dims = Dimensions::new(16, 64).expect("16,64>0");
			if cell.row.0 >= grid.dims().height() as u32 || cell.col.0 >= grid.dims().width() as u32
			{
				return Err(eyre::eyre!("Missing cell at row: {}, col: {}", row, col));
			}
			let mp = poly
				.multiproof(srs, &cell, &grid, target_dims)
				.map_err(|e| eyre::eyre!("Multiproof generation failed: {:?}", e))?;
			let data = mp
				.evals
				.into_iter()
				.flatten()
				.map(|e: ArkScalar| -> eyre::Result<GRawScalar> {
					let bytes = e
						.to_bytes()
						.map_err(|_| eyre::eyre!("Invalid scalar at row: {}", row))?;
					// Convert [u8; 32] to [u64; 4] by reading 8 bytes at a time
					let mut u64_array = [0u64; 4];
					for i in 0..4 {
						let start = i * 8;
						let end = start + 8;
						u64_array[i] = u64::from_le_bytes(bytes[start..end].try_into().unwrap());
					}
					Ok(GRawScalar(u64_array))
				})
				.collect::<eyre::Result<Vec<GRawScalar>>>()?;

			let proof_bytes: [u8; 48] = mp
				.proof
				.to_bytes()
				.map_err(|_| eyre::eyre!("Proof conversion failed"))?;
			let proof = GProof(proof_bytes);

			let gcell_block = GCellBlock {
				start_x: mp.block.start_x as u32,
				start_y: mp.block.start_y as u32,
				end_x: mp.block.end_x as u32,
				end_y: mp.block.end_y as u32,
			};
			Ok(((data, proof), gcell_block))
		})
		.collect::<eyre::Result<Vec<_>>>()?;

	Ok(proofs)
}
