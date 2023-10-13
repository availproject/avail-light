use avail_subxt::utils::H256;
use kate_recovery::matrix::{Dimensions, Position};
use rand::{thread_rng, Rng};
use std::{collections::HashSet, fmt::Display};
use tracing::debug;

use crate::consts::EXPECTED_NETWORK_VERSION;

mod client;
mod event_loop;

#[derive(Clone)]
pub struct Node {
	pub host: String,
	pub system_version: String,
	pub spec_version: u32,
	pub genesis_hash: H256,
}

impl Node {
	pub fn network(&self) -> String {
		format!(
			"{host}/{system_version}/{spec_name}/{spec_version}",
			host = self.host,
			system_version = self.system_version,
			spec_name = EXPECTED_NETWORK_VERSION.spec_name,
			spec_version = self.spec_version,
		)
	}
}

pub struct ExpectedVersion<'a> {
	pub version: &'a str,
	pub spec_name: &'a str,
}

impl ExpectedVersion<'_> {
	/// Checks if expected version matches network version.
	/// Since the light client uses subset of the node APIs, `matches` checks only prefix of a node version.
	/// This means that if expected version is `1.6`, versions `1.6.x` of the node will match.
	/// Specification name is checked for exact match.
	/// Since runtime `spec_version` can be changed with runtime upgrade, `spec_version` is removed.
	/// NOTE: Runtime compatibility check is currently not implemented.
	pub fn matches(&self, node_version: &str, spec_name: &str) -> bool {
		node_version.starts_with(self.version) && self.spec_name == spec_name
	}
}

impl Display for ExpectedVersion<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "v{}/{}", self.version, self.spec_name)
	}
}

/// Generates random cell positions for sampling
pub fn generate_random_cells(dimensions: Dimensions, cell_count: u32) -> Vec<Position> {
	let max_cells = dimensions.extended_size();
	let count = if max_cells < cell_count {
		debug!("Max cells count {max_cells} is lesser than cell_count {cell_count}");
		max_cells
	} else {
		cell_count
	};
	let mut rng = thread_rng();
	let mut indices = HashSet::new();
	while (indices.len() as u16) < count as u16 {
		let col = rng.gen_range(0..dimensions.cols().into());
		let row = rng.gen_range(0..dimensions.extended_rows());
		indices.insert(Position { row, col });
	}

	indices.into_iter().collect::<Vec<_>>()
}
