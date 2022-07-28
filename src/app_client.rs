use std::{
	collections::HashSet,
	sync::{mpsc::Receiver, Arc},
};

use anyhow::{anyhow, Context, Result};
use codec::{Compact, Decode, Error as DecodeError, Input};
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use ipfs_embed::{DefaultParams, Ipfs};
use kate_recovery::com::{
	app_specific_cells, app_specific_column_cells, decode_app_extrinsics,
	reconstruct_app_extrinsics, Cell, DataCell, ExtendedMatrixDimensions, Position,
};
use rocksdb::DB;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sp_runtime::{AccountId32, MultiAddress, MultiSignature};
use tracing::{error, info};

use crate::{
	data::{fetch_cells_from_dht, insert_into_dht, store_encoded_data_in_db},
	proof::verify_proof,
	rpc::get_kate_proof,
	types::{AppClientConfig, ClientMsg},
};

async fn process_block(
	cfg: &AppClientConfig,
	ipfs: &Ipfs<DefaultParams>,
	db: Arc<DB>,
	rpc_url: &str,
	app_id: u32,
	block: &ClientMsg,
	data_positions: &Vec<Position>,
	column_positions: &[Position],
	pp: PublicParameters,
) -> Result<()> {
	let block_number = block.number;

	info!(
		block_number,
		"Found {count} cells for app {app_id}",
		count = data_positions.len()
	);

	let (mut ipfs_cells, unfetched) = fetch_cells_from_dht(
		ipfs,
		block.number,
		data_positions,
		cfg.max_parallel_fetch_tasks,
	)
	.await
	.context("Failed to fetch data cells from DHT")?;

	info!(
		block_number,
		"Fetched {count} cells from DHT",
		count = ipfs_cells.len()
	);

	let mut rpc_cells: Vec<Cell> = Vec::new();
	if !cfg.disable_rpc {
		for cell in unfetched.chunks(30) {
			let mut query_cells = get_kate_proof(rpc_url, block.number, (*cell).to_vec())
				.await
				.context("Failed to fetch data cells from node RPC")?;

			rpc_cells.append(&mut query_cells);
		}
	}
	info!(
		block_number,
		"Fetched {count} cells from RPC",
		count = rpc_cells.len()
	);
	let reconstruct = data_positions.len() > (ipfs_cells.len() + rpc_cells.len());

	if reconstruct {
		let fetched = [ipfs_cells.as_slice(), rpc_cells.as_slice()].concat();
		let column_positions = diff_positions(column_positions, &fetched);
		let (mut column_ipfs_cells, unfetched) = fetch_cells_from_dht(
			ipfs,
			block_number,
			&column_positions,
			cfg.max_parallel_fetch_tasks,
		)
		.await
		.context("Failed to fetch column cells from IPFS")?;

		info!(
			block_number,
			"Fetched {count} cells from IPFS for reconstruction",
			count = column_ipfs_cells.len()
		);

		ipfs_cells.append(&mut column_ipfs_cells);
		let columns = columns(&column_positions);
		let fetched = [ipfs_cells.as_slice(), rpc_cells.as_slice()].concat();
		if !can_reconstruct(&block.dimensions, &columns, &fetched) && !cfg.disable_rpc {
			let mut column_rpc_cells = get_kate_proof(rpc_url, block_number, unfetched)
				.await
				.context("Failed to get column cells from node RPC")?;

			info!(
				block_number,
				"Fetched {count} cells from RPC for reconstruction",
				count = column_rpc_cells.len()
			);

			rpc_cells.append(&mut column_rpc_cells);
		}
	};

	let cells = [ipfs_cells.as_slice(), rpc_cells.as_slice()].concat();

	let verified = verify_proof(
		block_number,
		block.dimensions.rows as u16,
		block.dimensions.cols as u16,
		&cells,
		block.commitment.clone(),
		pp,
	);

	info!(block_number, "Completed {verified} verification rounds");

	if cells.len() > verified {
		return Err(anyhow!("{} cells are not verified", cells.len() - verified));
	}

	insert_into_dht(ipfs, block.number, rpc_cells, cfg.max_parallel_fetch_tasks).await;

	info!(block_number, "Cells inserted into IPFS");

	let data_cells = cells.into_iter().map(DataCell::from).collect::<Vec<_>>();
	let data = if reconstruct {
		reconstruct_app_extrinsics(&block.lookup, &block.dimensions, data_cells, app_id)
			.context("Failed to reconstruct app extrinsics")?
	} else {
		decode_app_extrinsics(&block.lookup, &block.dimensions, data_cells, app_id)
			.context("Failed to decode app extrinsics")?
	};

	store_encoded_data_in_db(db, app_id, block_number, &data)
		.context("Failed to store data into database")?;
	info!(
		block_number,
		"Stored {count} bytes into database",
		count = data.iter().fold(0usize, |acc, x| acc + x.len())
	);
	Ok(())
}

fn columns(positions: &[Position]) -> Vec<u16> {
	let columns = positions.iter().map(|position| position.col);
	HashSet::<u16>::from_iter(columns)
		.into_iter()
		.collect::<Vec<u16>>()
}

fn can_reconstruct(dimensions: &ExtendedMatrixDimensions, columns: &[u16], cells: &[Cell]) -> bool {
	columns.iter().all(|&col| {
		cells
			.iter()
			.filter(move |cell| cell.position.col == col)
			.count() >= dimensions.rows / 2
	})
}

fn diff_positions(positions: &[Position], cells: &[Cell]) -> Vec<Position> {
	positions
		.iter()
		.cloned()
		.filter(|position| !cells.iter().any(|cell| cell.position.eq(position)))
		.collect::<Vec<_>>()
}

pub async fn run(
	cfg: AppClientConfig,
	ipfs: Ipfs<DefaultParams>,
	db: Arc<DB>,
	rpc_url: String,
	app_id: u32,
	block_receive: Receiver<ClientMsg>,
	pp: PublicParameters,
) {
	info!("Starting for app {app_id}...");

	for block in block_receive {
		let block_number = block.number;
		info!(block_number, "Block available");

		match (
			app_specific_cells(&block.lookup, &block.dimensions, app_id),
			app_specific_column_cells(&block.lookup, &block.dimensions, app_id),
		) {
			(Some(data_positions), Some(column_positions)) => {
				if let Err(error) = process_block(
					&cfg,
					&ipfs,
					db.clone(),
					&rpc_url,
					app_id,
					&block,
					&data_positions,
					&column_positions,
					pp.clone(),
				)
				.await
				{
					error!(block_number, "Cannot process block: {error}");
					continue;
				}
			},
			(_, _) => info!(block_number, "No cells for app {app_id}"),
		}
	}
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct AvailExtrinsic {
	pub app_id: u32,
	pub signature: Option<MultiSignature>,
	#[serde(serialize_with = "as_string")]
	pub data: Vec<u8>,
}

fn as_string<S>(t: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
where
	S: Serializer,
{
	sp_core::bytes::serialize(t, serializer)
}

pub type AvailSignedExtra = ((), (), (), AvailMortality, Nonce, (), Balance, u32);

#[derive(Decode)]
pub struct Balance(#[codec(compact)] u128);

#[derive(Decode)]
pub struct Nonce(#[codec(compact)] u32);

pub enum AvailMortality {
	Immortal,
	Mortal(u64, u64),
}

impl Decode for AvailMortality {
	fn decode<I: Input>(input: &mut I) -> Result<Self, DecodeError> {
		let first = input.read_byte()?;
		if first == 0 {
			Ok(Self::Immortal)
		} else {
			let encoded = first as u64 + ((input.read_byte()? as u64) << 8);
			let period = 2 << (encoded % (1 << 4));
			let quantize_factor = (period >> 12).max(1);
			let phase = (encoded >> 4) * quantize_factor;
			if period >= 4 && phase < period {
				Ok(Self::Mortal(period, phase))
			} else {
				Err("Invalid period and phase".into())
			}
		}
	}
}

const EXTRINSIC_VERSION: u8 = 4;
impl Decode for AvailExtrinsic {
	fn decode<I: Input>(input: &mut I) -> Result<AvailExtrinsic, DecodeError> {
		// This is a little more complicated than usual since the binary format must be compatible
		// with substrate's generic `Vec<u8>` type. Basically this just means accepting that there
		// will be a prefix of vector length (we don't need
		// to use this).
		let _length_do_not_remove_me_see_above: Compact<u32> = Decode::decode(input)?;

		let version = input.read_byte()?;

		let is_signed = version & 0b1000_0000 != 0;
		let version = version & 0b0111_1111;
		if version != EXTRINSIC_VERSION {
			return Err("Invalid transaction version".into());
		}
		let (app_id, signature) = if is_signed {
			let _address = <MultiAddress<AccountId32, u32>>::decode(input)?;
			let sig = MultiSignature::decode(input)?;
			let extra = <AvailSignedExtra>::decode(input)?;
			let app_id = extra.7;

			(app_id, Some(sig))
		} else {
			return Err("Not signed".into());
		};

		let section: u8 = Decode::decode(input)?;
		let method: u8 = Decode::decode(input)?;

		let data: Vec<u8> = match (section, method) {
			// TODO: Define these pairs as enums or better yet - make a dependency on substrate enums if possible
			(29, 1) => Decode::decode(input)?,
			_ => return Err("Not Avail Extrinsic".into()),
		};

		Ok(Self {
			app_id,
			signature,
			data,
		})
	}
}

impl<'a> Deserialize<'a> for AvailExtrinsic {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'a>,
	{
		let r = sp_core::bytes::deserialize(deserializer)?;
		Decode::decode(&mut &r[..])
			.map_err(|e| serde::de::Error::custom(format!("Decode error: {}", e)))
	}
}

#[cfg(test)]
mod tests {
	use kate_recovery::com::{Cell, ExtendedMatrixDimensions, Position};

	use super::{can_reconstruct, diff_positions, AvailExtrinsic};

	fn position(row: u16, col: u16) -> Position { Position { row, col } }

	fn empty_cell(row: u16, col: u16) -> Cell {
		Cell {
			position: Position { row, col },
			content: [0u8; 80],
		}
	}

	#[test]
	fn test_can_reconstruct() {
		let dimensions = ExtendedMatrixDimensions { rows: 2, cols: 4 };
		let columns = vec![0, 1];
		let cells = vec![empty_cell(0, 0), empty_cell(0, 1)];
		assert!(can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 0), empty_cell(0, 1)];
		assert!(can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 0), empty_cell(1, 1)];
		assert!(can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(0, 0), empty_cell(1, 1)];
		assert!(can_reconstruct(&dimensions, &columns, &cells));
	}

	#[test]
	fn test_cannot_reconstruct() {
		let dimensions = ExtendedMatrixDimensions { rows: 2, cols: 4 };
		let columns = vec![0, 1];
		let cells = vec![empty_cell(0, 0)];
		assert!(!can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(0, 1)];
		assert!(!can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 0)];
		assert!(!can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 1)];
		assert!(!can_reconstruct(&dimensions, &columns, &cells));
		let cells = vec![empty_cell(0, 2), empty_cell(0, 3)];
		assert!(!can_reconstruct(&dimensions, &columns, &cells));
	}

	#[test]
	fn test_diff_positions() {
		let positions = vec![position(0, 0), position(1, 1)];
		let cells = vec![empty_cell(0, 0), empty_cell(1, 1)];
		assert_eq!(diff_positions(&positions, &cells).len(), 0);

		let positions = vec![position(0, 0), position(1, 1)];
		let cells = vec![empty_cell(0, 0), empty_cell(0, 1)];
		assert_eq!(diff_positions(&positions, &cells).len(), 1);
		assert_eq!(diff_positions(&positions, &cells)[0], position(1, 1));

		let positions = vec![position(0, 0), position(1, 1)];
		let cells = vec![empty_cell(1, 0), empty_cell(0, 1)];
		assert_eq!(diff_positions(&positions, &cells).len(), 2);
		assert_eq!(diff_positions(&positions, &cells)[0], position(0, 0));
		assert_eq!(diff_positions(&positions, &cells)[1], position(1, 1));
	}

	#[test]
	fn test_decode_xt() {
		let xt= serde_json::to_string("0xe9018400de1113c5912fda9c77305cddd98e2b5ca156f260ff2ac329dde67110854f8f3901007a35bdd5ec15a69bcd37d648dafcf18693f158baca512be44f1dfc218048581ba527938763b9b5a16f915e29c101c8450a2dd04d795de704f496ce9c81038d00dd1900030000001d01306578616d706c652064617461").unwrap();
		let x: AvailExtrinsic = serde_json::from_str(&xt).unwrap();
		let id = x.app_id;
		let data = String::from_utf8_lossy(x.data.as_slice());
		assert_eq!(id, 3);
		assert_eq!(data, "example data");
		println!("id: {id}, data: {data}.");
	}
}
