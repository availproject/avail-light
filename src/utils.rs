use kate_recovery::{
	data::Cell,
	matrix::{Dimensions, Position},
};

// TODO: Remove unused functions if not needed after next iteration

#[allow(dead_code)]
fn can_reconstruct(dimensions: &Dimensions, columns: &[u16], cells: &[Cell]) -> bool {
	columns.iter().all(|&col| {
		cells
			.iter()
			.filter(move |cell| cell.position.col == col)
			.count() as u16
			>= dimensions.rows
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
		let dimensions = Dimensions { rows: 1, cols: 4 };
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
		let dimensions = Dimensions { rows: 1, cols: 4 };
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
}
