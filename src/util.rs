pub fn layout_from_index(index: &[(u32, u32)], size: u32) -> Vec<(u32, u32)> {
	if index.is_empty() {
		return vec![(0, size)];
	}

	let (app_ids, offsets): (Vec<_>, Vec<_>) = index.iter().cloned().unzip();
	// Prepend app_id zero
	let mut app_ids_ext = vec![0];
	app_ids_ext.extend(app_ids);

	// Prepend offset 0 for app_id 0
	let mut offsets_ext = vec![0];
	offsets_ext.extend(offsets);

	let mut sizes = offsets_ext[0..offsets_ext.len() - 1]
		.iter()
		.zip(offsets_ext[1..].iter())
		.map(|(a, b)| b - a)
		.collect::<Vec<_>>();

	let remaining_size: u32 = size - sizes.iter().sum::<u32>();
	sizes.push(remaining_size);

	app_ids_ext.into_iter().zip(sizes.into_iter()).collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_layout_from_index() {
		let expected = vec![(0, 3), (1, 3), (2, 4)];
		assert_eq!(layout_from_index(&[(1, 3), (2, 6)], 10), expected);

		let expected = vec![(0, 12), (1, 3), (3, 5)];
		assert_eq!(layout_from_index(&[(1, 12), (3, 15)], 20), expected);

		let expected = vec![(0, 1), (1, 5)];
		assert_eq!(layout_from_index(&[(1, 1)], 6), expected);
	}
}
