extern crate anyhow;
extern crate ipfs_embed;
extern crate libipld;

use anyhow::{Context, Result};
use futures::future::join_all;
use ipfs_embed::{Cid, DefaultParams, Ipfs, Key, PeerId};
use kate_recovery::com::Position;

use crate::types::Cell as DataCell;

async fn fetch_cell_from_ipfs(
	ipfs: &Ipfs<DefaultParams>,
	peers: Vec<PeerId>,
	block_number: u64,
	position: &Position,
) -> Result<DataCell> {
	let reference = position.reference(block_number);
	let record_key = Key::from(reference.as_bytes().to_vec());

	log::trace!("Getting DHT record for reference {}", reference);
	let cid = ipfs
		.get_record(record_key, ipfs_embed::Quorum::One)
		.await
		.map(|record| record[0].record.value.to_vec())
		.and_then(|value| Cid::try_from(value).context("Invalid CID value"))?;

	log::trace!("Fetching IPFS block for CID {}", cid);
	ipfs.fetch(&cid, peers).await.and_then(|block| {
		Ok(DataCell {
			block: block_number,
			position: position.clone(),
			proof: block.data().to_vec(),
			data: block.data()[48..].to_vec(),
		})
	})
}

pub async fn fetch_cells_from_ipfs(
	ipfs: &Ipfs<DefaultParams>,
	block_number: u64,
	positions: &Vec<Position>,
) -> Result<(Vec<DataCell>, Vec<Position>)> {
	// TODO: Should we fetch peers before loop or for each cell?
	let peers = &ipfs.peers();
	log::debug!("Number of known IPFS peers: {}", peers.len());

	if peers.is_empty() {
		log::info!("No known IPFS peers");
		return Ok((vec![], positions.to_vec()));
	}

	let res = join_all(
		positions
			.iter()
			.map(|position| fetch_cell_from_ipfs(ipfs, peers.clone(), block_number, position))
			.collect::<Vec<_>>(),
	)
	.await;

	let (fetched, unfetched): (Vec<_>, Vec<_>) = res
		.into_iter()
		.zip(positions)
		.partition(|(res, _)| res.is_ok());

	let fetched = fetched
		.into_iter()
		.map(|e| {
			let cell = e.0.unwrap();
			log::debug!("Fetched cell {} from IPFS", cell.reference());
			cell
		})
		.collect::<Vec<_>>();

	let unfetched = unfetched
		.into_iter()
		.map(|(result, position)| {
			log::debug!(
				"Error fetching cell {} from IPFS: {}",
				position.reference(block_number),
				result.unwrap_err()
			);
			position.clone()
		})
		.collect::<Vec<_>>();

	log::info!("Number of cells fetched from IPFS: {}", fetched.len());
	Ok((fetched, unfetched))
}

#[cfg(test)]
mod tests {
	// TODO
}
