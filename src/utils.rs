use avail_core::{
	data_lookup::compact::{CompactDataLookup, DataLookupItem},
	data_lookup::Error as DataLookupError,
	AppId, DataLookup,
};
use avail_subxt::{
	api::runtime_types::{
		avail_core::{header::extension::v3, header::extension::HeaderExtension},
		da_control::pallet::Call,
		da_runtime::RuntimeCall,
	},
	primitives::{
		grandpa::AuthorityId, grandpa::ConsensusLog, AppUncheckedExtrinsic, Header as DaHeader,
	},
	utils::H256,
};
use codec::Decode;
use color_eyre::{eyre::WrapErr, Result};
use kate_recovery::{
	data::Cell,
	matrix::{Dimensions, Position},
};

pub fn decode_app_data(data: &[u8]) -> Result<Option<Vec<u8>>> {
	let extrisic: AppUncheckedExtrinsic =
		<_ as Decode>::decode(&mut &data[..]).wrap_err("Couldn't decode AvailExtrinsic")?;

	match extrisic.function {
		RuntimeCall::DataAvailability(Call::submit_data { data, .. }) => Ok(Some(data.0)),
		_ => Ok(None),
	}
}

/// Calculates confidence from given number of verified cells
pub fn calculate_confidence(count: u32) -> f64 {
	100f64 * (1f64 - 1f64 / 2u32.pow(count) as f64)
}

/// Extract fields from extension header
pub(crate) fn extract_kate(extension: &HeaderExtension) -> (u16, u16, H256, Vec<u8>) {
	match &extension {
		HeaderExtension::V3(v3::HeaderExtension {
			commitment: kate, ..
		}) => (
			kate.rows,
			kate.cols,
			kate.data_root,
			kate.commitment.clone(),
		),
	}
}

pub(crate) fn extract_app_lookup(
	extension: &HeaderExtension,
) -> Result<DataLookup, DataLookupError> {
	let compact = match &extension {
		HeaderExtension::V3(v3::HeaderExtension { app_lookup, .. }) => app_lookup,
	};

	let size = compact.size;
	let index = compact
		.index
		.iter()
		.map(|item| DataLookupItem::new(AppId(item.app_id.0), item.start))
		.collect::<Vec<_>>();

	let compact = CompactDataLookup::new(size, index);
	DataLookup::try_from(compact)
}

pub fn filter_auth_set_changes(header: &DaHeader) -> Vec<Vec<(AuthorityId, u64)>> {
	let new_auths = header
		.digest
		.logs
		.iter()
		.filter_map(|e| match &e {
			// UGHHH, why won't the b"FRNK" just work
			avail_subxt::config::substrate::DigestItem::Consensus(
				[b'F', b'R', b'N', b'K'],
				data,
			) => match ConsensusLog::<u32>::decode(&mut data.as_slice()) {
				Ok(ConsensusLog::ScheduledChange(x)) => Some(x.next_authorities),
				Ok(ConsensusLog::ForcedChange(_, x)) => Some(x.next_authorities),
				_ => None,
			},
			_ => None,
		})
		.collect::<Vec<_>>();
	new_auths
}

// TODO: Remove unused functions if not needed after next iteration

#[allow(dead_code)]
fn can_reconstruct(dimensions: Dimensions, columns: &[u16], cells: &[Cell]) -> bool {
	columns.iter().all(|&col| {
		cells
			.iter()
			.filter(move |cell| cell.position.col == col)
			.count() as u16
			>= dimensions.rows().into()
	})
}

#[allow(dead_code)]
fn diff_positions(positions: &[Position], cells: &[Cell]) -> Vec<Position> {
	positions
		.iter()
		.cloned()
		.filter(|position| !cells.iter().any(|cell| cell.position.eq(position)))
		.collect::<Vec<_>>()
}

#[cfg(test)]
mod tests {
	use super::{can_reconstruct, diff_positions};
	use kate_recovery::{
		data::Cell,
		matrix::{Dimensions, Position},
	};

	fn position(row: u32, col: u16) -> Position {
		Position { row, col }
	}

	fn empty_cell(row: u32, col: u16) -> Cell {
		Cell {
			position: Position { row, col },
			content: [0u8; 80],
		}
	}

	#[test]
	fn test_can_reconstruct() {
		let dimensions = Dimensions::new(1, 4).unwrap();
		let columns = vec![0, 1];
		let cells = vec![empty_cell(0, 0), empty_cell(0, 1)];
		assert!(can_reconstruct(dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 0), empty_cell(0, 1)];
		assert!(can_reconstruct(dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 0), empty_cell(1, 1)];
		assert!(can_reconstruct(dimensions, &columns, &cells));
		let cells = vec![empty_cell(0, 0), empty_cell(1, 1)];
		assert!(can_reconstruct(dimensions, &columns, &cells));
	}

	#[test]
	fn test_cannot_reconstruct() {
		let dimensions = Dimensions::new(1, 4).unwrap();
		let columns = vec![0, 1];
		let cells = vec![empty_cell(0, 0)];
		assert!(!can_reconstruct(dimensions, &columns, &cells));
		let cells = vec![empty_cell(0, 1)];
		assert!(!can_reconstruct(dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 0)];
		assert!(!can_reconstruct(dimensions, &columns, &cells));
		let cells = vec![empty_cell(1, 1)];
		assert!(!can_reconstruct(dimensions, &columns, &cells));
		let cells = vec![empty_cell(0, 2), empty_cell(0, 3)];
		assert!(!can_reconstruct(dimensions, &columns, &cells));
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
}
