#[cfg(target_arch = "wasm32")]
pub use wasm::{verify, Error};

#[cfg(not(target_arch = "wasm32"))]
#[allow(deprecated)]
pub use avail_rust::kate_recovery::proof::{verify, Error};

#[cfg(target_arch = "wasm32")]
mod wasm {
	use avail_rust::{
		avail_core::kate::COMMITMENT_SIZE,
		kate_recovery::{data::SingleCell, matrix::Dimensions},
	};
	use dusk_bytes::Serializable;
	use dusk_plonk::{
		bls12_381::G1Affine,
		commitment_scheme::kzg10::{commitment::Commitment, proof::Proof, PublicParameters},
		fft::EvaluationDomain,
		prelude::BlsScalar,
	};
	use thiserror_no_std::Error;

	impl std::error::Error for Error {}

	impl From<dusk_bytes::Error> for Error {
		fn from(_: dusk_bytes::Error) -> Self {
			Error::InvalidData
		}
	}

	#[derive(Error, Debug)]
	pub enum Error {
		#[error("Proof, data or commitment is not valid")]
		InvalidData,
		#[error("Evaluation domain is not valid for given dimensions")]
		InvalidDomain,
		#[error("Public parameters degree is to small for given dimensions")]
		InvalidDegree,
		#[error("Position isn't in domain")]
		InvalidPositionInDomain,
	}

	pub fn verify(
		public_parameters: &PublicParameters,
		dimensions: Dimensions,
		commitment: &[u8; COMMITMENT_SIZE],
		cell: &SingleCell,
	) -> Result<bool, Error> {
		let width: u16 = dimensions.cols().into();
		let commitment_to_witness = G1Affine::from_bytes(&cell.proof()).map(Commitment::from)?;

		let evaluated_point = BlsScalar::from_bytes(&cell.data())?;

		let commitment_to_polynomial = G1Affine::from_bytes(commitment).map(Commitment::from)?;

		let proof = Proof {
			commitment_to_witness,
			evaluated_point,
			commitment_to_polynomial,
		};

		let cols: usize = usize::from(width);
		let point = EvaluationDomain::new(cols)
			.map_err(|_| Error::InvalidDomain)?
			.elements()
			.nth(cell.position.col.into())
			.ok_or(Error::InvalidPositionInDomain)?;

		public_parameters
			.trim(cols)
			.map(|(_, verifier_key)| verifier_key.check(point, proof))
			.map_err(|_| Error::InvalidDegree)
	}
}
