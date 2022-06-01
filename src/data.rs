extern crate anyhow;
extern crate ipfs_embed;
extern crate libipld;

use anyhow::{Context, Result};
use ipfs_embed::{Cid, DefaultParams, Ipfs, Key, PeerId};

use crate::types::Cell as DataCell;

async fn fetch_cell_from_ipfs(
	ipfs: &Ipfs<DefaultParams>,
	peers: Vec<PeerId>,
	block_number: u64,
	position: &DataCell,
) -> Result<DataCell> {
	let record_key = Key::from(position.reference().as_bytes().to_vec());

	log::trace!("Getting DHT record for reference {}", position.reference());
	let cid = ipfs
		.get_record(record_key, ipfs_embed::Quorum::One)
		.await
		.map(|record| record[0].record.value.to_vec())
		.and_then(|value| Cid::try_from(value).context("Invalid CID value"))?;

	log::trace!("Fetching IPFS block for CID {}", cid);
	ipfs.fetch(&cid, peers).await.and_then(|block| {
		Ok(DataCell {
			block: block_number,
			row: position.row,
			col: position.col,
			proof: block.data().to_vec(),
			data: block.data()[48..].to_vec(),
		})
	})
}

pub async fn fetch_cells_from_ipfs(
	ipfs: &Ipfs<DefaultParams>,
	block_number: u64,
	positions: &Vec<DataCell>,
) -> Result<(Vec<DataCell>, Vec<DataCell>)> {
	// TODO: Should we fetch peers before loop or for each cell?
	let peers = ipfs.peers();
	log::debug!("Number of known IPFS peers: {}", peers.len());

	if peers.is_empty() {
		log::info!("No known IPFS peers");
		return Ok((vec![], positions.to_vec()));
	}

	let mut fetched = vec![];
	let mut unfetched = vec![];
	// TODO: Paralelize fetching
	for position in positions {
		match fetch_cell_from_ipfs(ipfs, peers.clone(), block_number, position).await {
			Ok(cell) => {
				log::debug!("Fetched cell {} from IPFS", position.reference());
				fetched.push(cell);
			},
			Err(error) => {
				// Skip cells that returns error when fetching from IPFS
				log::debug!(
					"Error fetching cell {} from IPFS: {}",
					position.reference(),
					error
				);
				unfetched.push(position.clone());
				continue;
			},
		}
	}

	log::info!("Number of cells fetched from IPFS: {}", fetched.len());
	Ok((fetched, unfetched))
}

#[cfg(test)]
mod tests {
	// TODO
}
