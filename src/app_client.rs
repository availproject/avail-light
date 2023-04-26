//! Application client for data fetching and reconstruction.
//!
//! App client is enabled when app_id is configured and greater than 0 in avail-light configuration. [`Light client`](super::light_client) triggers application client if block is verified with high enough confidence. Currently [`run`] function is separate task and doesn't block main thread.
//!
//! # Flow
//!
//! Get app data rows from node
//! Verify commitment equality for each row
//! Decode app data and store it into local database under the `app_id:block_number` key
//!
//! # Notes
//!
//! If application client fails to run or stops its execution, error is logged, and other tasks continue with execution.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use avail_subxt::AvailConfig;
use codec::Encode;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use kate_recovery::{
	com::{app_specific_rows, columns_positions, decode_app_extrinsics, reconstruct_columns},
	commitments,
	config::{self, CHUNK_SIZE},
	data::{Cell, DataCell},
	matrix::{Dimensions, Position},
};
use mockall::automock;
use rocksdb::DB;
use std::{
	collections::{HashMap, HashSet},
	sync::Arc,
};
use subxt::{utils::H256, OnlineClient};
use tokio::sync::mpsc::Receiver;
use tracing::{debug, error, info, instrument};

use crate::{
	data::store_encoded_data_in_db,
	network::Client,
	proof, rpc,
	types::{AppClientConfig, BlockVerified},
};

#[async_trait]
#[automock]
pub trait AppClient {
	async fn reconstruct_rows_from_dht(
		&self,
		pp: PublicParameters,
		block_number: u32,
		dimensions: &Dimensions,
		commitments: &[[u8; config::COMMITMENT_SIZE]],
		missing_rows: &[u32],
	) -> Result<Vec<(u32, Vec<u8>)>>;

	async fn fetch_rows_from_dht(
		&self,
		block_number: u32,
		dimensions: &Dimensions,
		row_indexes: &[u32],
	) -> Vec<Option<Vec<u8>>>;

	async fn get_kate_rows(&self, rows: Vec<u32>, block_hash: H256)
		-> Result<Vec<Option<Vec<u8>>>>;

	fn store_encoded_data_in_db<T: Encode + 'static>(
		&self,
		app_id: u32,
		block_number: u32,
		data: &T,
	) -> Result<()>;
}

#[derive(Clone)]
pub struct AppClientImpl {
	db: Arc<DB>,
	network_client: Client,
	rpc_client: OnlineClient<AvailConfig>,
}

#[async_trait]
impl AppClient for AppClientImpl {
	async fn reconstruct_rows_from_dht(
		&self,
		pp: PublicParameters,
		block_number: u32,
		dimensions: &Dimensions,
		commitments: &[[u8; config::COMMITMENT_SIZE]],
		missing_rows: &[u32],
	) -> Result<Vec<(u32, Vec<u8>)>> {
		let missing_cells = dimensions.extended_rows_positions(missing_rows);

		if missing_cells.is_empty() {
			return Ok(vec![]);
		}

		debug!(
			block_number,
			"Fetching {} missing row cells from DHT",
			missing_cells.len()
		);
		let (fetched, unfetched) = fetch_verified(
			&pp,
			&self.network_client,
			block_number,
			dimensions,
			commitments,
			&missing_cells,
		)
		.await?;
		debug!(
			block_number,
			"Fetched {} row cells, {} row cells is missing",
			fetched.len(),
			unfetched.len()
		);

		let missing_cells = columns_positions(dimensions, &unfetched, 0.66);

		let (missing_fetched, _) = fetch_verified(
			&pp,
			&self.network_client,
			block_number,
			dimensions,
			commitments,
			&missing_cells,
		)
		.await?;

		let reconstructed = reconstruct_columns(dimensions, &missing_fetched)?;

		debug!(
			block_number,
			"Reconstructed {} columns: {:?}",
			reconstructed.keys().len(),
			reconstructed.keys()
		);

		let mut reconstructed_cells = unfetched
			.into_iter()
			.map(|position| data_cell(position, &reconstructed))
			.collect::<Result<Vec<_>>>()?;

		debug!(
			block_number,
			"Reconstructed {} missing row cells",
			reconstructed_cells.len()
		);

		let mut data_cells: Vec<DataCell> = fetched.into_iter().map(Into::into).collect::<Vec<_>>();

		data_cells.append(&mut reconstructed_cells);

		data_cells.sort_by(|a, b| {
			(a.position.row, a.position.col).cmp(&(b.position.row, b.position.col))
		});

		missing_rows
			.iter()
			.map(|&row| {
				let data = data_cells
					.iter()
					.filter(|&cell| cell.position.row == row)
					.flat_map(|cell| cell.data)
					.collect::<Vec<_>>();

				if data.len() != dimensions.cols() as usize * config::CHUNK_SIZE {
					return Err(anyhow!("Row size is not valid after reconstruction"));
				}

				Ok((row, data))
			})
			.collect::<Result<Vec<_>>>()
	}

	async fn fetch_rows_from_dht(
		&self,
		block_number: u32,
		dimensions: &Dimensions,
		row_indexes: &[u32],
	) -> Vec<Option<Vec<u8>>> {
		self.network_client
			.fetch_rows_from_dht(block_number, dimensions, row_indexes)
			.await
	}

	async fn get_kate_rows(
		&self,
		rows: Vec<u32>,
		block_hash: H256,
	) -> Result<Vec<Option<Vec<u8>>>> {
		Ok(rpc::get_kate_rows(&self.rpc_client, rows, block_hash).await?)
	}

	fn store_encoded_data_in_db<T: Encode + 'static>(
		&self,
		app_id: u32,
		block_number: u32,
		data: &T,
	) -> Result<()> {
		store_encoded_data_in_db(self.db.clone(), app_id, block_number, &data)
			.context("Failed to store data into database")?;
		Ok(())
	}
}

fn new_data_cell(row: usize, col: usize, data: &[u8]) -> Result<DataCell> {
	Ok(DataCell {
		position: Position {
			row: row.try_into()?,
			col: col.try_into()?,
		},
		data: data.try_into()?,
	})
}

fn data_cells_from_row(row: usize, row_data: &[u8]) -> Result<Vec<DataCell>> {
	row_data
		.chunks_exact(CHUNK_SIZE)
		.enumerate()
		.map(move |(col, data)| new_data_cell(row, col, data))
		.collect::<Result<Vec<DataCell>>>()
}

fn data_cells_from_rows(rows: Vec<Option<Vec<u8>>>) -> Result<Vec<DataCell>> {
	Ok(rows
		.into_iter()
		.enumerate() // Add row indexes
		.filter_map(|(row, row_data)| row_data.map(|data| (row, data))) // Remove None rows
		.map(|(row, data)| data_cells_from_row(row, &data))
		.collect::<Result<Vec<Vec<DataCell>>, _>>()?
		.into_iter()
		.flatten()
		.collect::<Vec<_>>())
}

fn data_cell(
	position: Position,
	reconstructed: &HashMap<u16, Vec<[u8; config::CHUNK_SIZE]>>,
) -> Result<DataCell> {
	let row: usize = position.row.try_into()?;
	reconstructed
		.get(&position.col)
		// Dividing with extension factor since reconstracted column is not extended
		.and_then(|column| column.get(row / config::EXTENSION_FACTOR))
		.map(|&data| DataCell { position, data })
		.context("Data cell not found")
}

async fn fetch_verified(
	pp: &PublicParameters,
	network_client: &Client,
	block_number: u32,
	dimensions: &Dimensions,
	commitments: &[[u8; config::COMMITMENT_SIZE]],
	positions: &[Position],
) -> Result<(Vec<Cell>, Vec<Position>)> {
	let (mut fetched, mut unfetched) = network_client
		.fetch_cells_from_dht(block_number, positions)
		.await;

	let (verified, mut unverified) =
		proof::verify(block_number, dimensions, &fetched, commitments, pp)
			.context("Failed to verify fetched cells")?;

	fetched.retain(|cell| verified.contains(&cell.position));
	unfetched.append(&mut unverified);

	Ok((fetched, unfetched))
}

#[instrument(skip_all, fields(block = block.block_num), level = "trace")]
async fn process_block<T>(
	app_client: T,
	cfg: &AppClientConfig,
	// db: Arc<DB>,
	// network_client: Client,
	// rpc_client: &OnlineClient<AvailConfig>,
	app_id: u32,
	block: &BlockVerified,
	pp: PublicParameters,
) -> Result<()>
where
	T: AppClient,
{
	let lookup = &block.lookup;
	let block_number = block.block_num;
	let dimensions = &block.dimensions;

	let commitments = &block.commitments;

	let app_rows = app_specific_rows(lookup, dimensions, app_id);

	debug!(
		block_number,
		"Fetching {} app rows from DHT: {app_rows:?}",
		app_rows.len()
	);

	let dht_rows = app_client
		.fetch_rows_from_dht(block_number, dimensions, &app_rows)
		.await;
	let dht_rows_count = dht_rows.iter().flatten().count();
	debug!(block_number, "Fetched {dht_rows_count} app rows from DHT");

	let (dht_verified_rows, dht_missing_rows) =
		commitments::verify_equality(&pp, commitments, &dht_rows, lookup, dimensions, app_id)?;
	debug!(
		block_number,
		"Verified {} app rows from DHT, missing {}",
		dht_verified_rows.len(),
		dht_missing_rows.len()
	);

	let rpc_rows = if cfg.disable_rpc {
		vec![None; dht_rows.len()]
	} else {
		debug!(
			block_number,
			"Fetching missing app rows from RPC: {dht_missing_rows:?}",
		);
		app_client
			.get_kate_rows(dht_missing_rows, block.header_hash)
			.await?
	};

	let (rpc_verified_rows, mut missing_rows) =
		commitments::verify_equality(&pp, commitments, &rpc_rows, lookup, dimensions, app_id)?;
	// Since verify_equality returns all missing rows, exclude DHT rows that are already verified
	missing_rows.retain(|row| !dht_verified_rows.contains(row));

	debug!(
		block_number,
		"Verified {} app rows from RPC, missing {}",
		rpc_verified_rows.len(),
		missing_rows.len()
	);

	let verified_rows_iter = dht_verified_rows
		.into_iter()
		.chain(rpc_verified_rows.into_iter());
	let verified_rows: HashSet<u32> = HashSet::from_iter(verified_rows_iter);

	let mut rows = dht_rows
		.into_iter()
		.zip(rpc_rows.into_iter())
		.zip(0..dimensions.extended_rows())
		.map(|((dht_row, rpc_row), row_index)| {
			let row = dht_row.or(rpc_row)?;
			verified_rows.contains(&row_index).then_some(row)
		})
		.collect::<Vec<_>>();

	let rows_count = rows.iter().filter(|row| row.is_some()).count();
	debug!(
		block_number,
		"Found {rows_count} rows, verified {}, {} is missing",
		verified_rows.len(),
		missing_rows.len()
	);

	if missing_rows.len() * dimensions.cols() as usize > cfg.threshold {
		return Err(anyhow::anyhow!("Too many cells are missing"));
	}

	debug!(
		block_number,
		"Reconstructing {} missing app rows from DHT: {missing_rows:?}",
		missing_rows.len()
	);

	let dht_rows = app_client
		.reconstruct_rows_from_dht(pp, block_number, dimensions, commitments, &missing_rows)
		.await?;
	debug!(
		block_number,
		"Reconstructed {} app rows from DHT",
		dht_rows.len()
	);

	for (row_index, row) in dht_rows {
		let i: usize = row_index.try_into()?;
		rows[i] = Some(row);
	}

	let data_cells =
		data_cells_from_rows(rows).context("Failed to create data cells from rows got from RPC")?;

	let data = decode_app_extrinsics(lookup, dimensions, data_cells, app_id)
		.context("Failed to decode app extrinsics")?;

	debug!(block_number, "Storing data into database");
	app_client
		.store_encoded_data_in_db(app_id, block_number, &data)
		.context("Failed to store data into database")?;

	let bytes_count = data.iter().fold(0usize, |acc, x| acc + x.len());
	debug!(block_number, "Stored {bytes_count} bytes into database");

	Ok(())
}

/// Runs application client.
///
/// # Arguments
///
/// * `cfg` - Application client configuration
/// * `db` - Database to store data inot DB
/// * `network_client` - Reference to a libp2p custom network client
/// * `rpc_client` - Node's RPC subxt client for fetching data unavailable in DHT (if configured)
/// * `app_id` - Application ID
/// * `block_receive` - Channel used to receive header of verified block
/// * `pp` - Public parameters (i.e. SRS) needed for proof verification
pub async fn run(
	cfg: AppClientConfig,
	db: Arc<DB>,
	network_client: Client,
	rpc_client: OnlineClient<AvailConfig>,
	app_id: u32,
	mut block_receive: Receiver<BlockVerified>,
	pp: PublicParameters,
) {
	info!("Starting for app {app_id}...");

	while let Some(block) = block_receive.recv().await {
		let block_number = block.block_num;
		let dimensions = &block.dimensions;

		info!(block_number, "Block available: {dimensions:?}");

		if block.dimensions.cols() == 0 {
			info!(block_number, "Skipping empty block");
			continue;
		}

		if block
			.lookup
			.index
			.iter()
			.filter(|&(id, _)| id == &app_id)
			.count() == 0
		{
			info!(
				block_number,
				"Skipping block with no cells for app {app_id}"
			);
			continue;
		}

		let db_clone = db.clone();
		let app_client = AppClientImpl {
			db: db_clone,
			network_client: network_client.clone(),
			rpc_client: rpc_client.clone(),
		};
		if let Err(error) = process_block(app_client, &cfg, app_id, &block, pp.clone()).await {
			error!(block_number, "Cannot process block: {error}");
		} else {
			debug!(block_number, "Block processed");
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::types::{AppClientConfig, RuntimeConfig};
	use hex_literal::hex;
	use kate_recovery::{index::AppDataIndex, matrix::Dimensions, testnet};

	#[tokio::test]
	async fn test_process_blocks() {
		let cfg = AppClientConfig::from(&RuntimeConfig::default());
		let pp = testnet::public_params(1024);
		let dimensions: Dimensions = Dimensions::new(1, 16).unwrap();
		let mut mock_client = MockAppClient::new();
		// let dht_missing_rows: Vec<u32> = vec![0];
		let dht_rows: Vec<Option<Vec<u8>>> = vec![None, None];
		let kate_rows: Vec<Option<Vec<u8>>> = [
			Some(vec![
				4, 44, 40, 4, 3, 0, 11, 163, 250, 10, 184, 135, 1, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0,
				0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 217, 4, 209, 4, 132, 0, 212, 53, 147, 199, 21, 253,
				211, 28, 97, 20, 26, 189, 4, 169, 159, 214, 130, 44, 133, 88, 133, 76, 205, 227, 0,
				154, 86, 132, 231, 165, 109, 162, 125, 1, 168, 207, 88, 225, 233, 199, 53, 249, 62,
				188, 122, 148, 8, 106, 162, 124, 253, 119, 219, 23, 58, 172, 0, 128, 56, 149, 136,
				107, 138, 79, 73, 232, 92, 104, 244, 105, 213, 112, 240, 237, 153, 39, 80, 191,
				149, 50, 155, 185, 14, 245, 107, 69, 171, 205, 0, 159, 237, 239, 13, 156, 189, 214,
				28, 5, 161, 129, 212, 1, 56, 0, 4, 29, 1, 33, 3, 48, 54, 52, 50, 101, 53, 100, 48,
				52, 98, 54, 0, 54, 50, 53, 57, 102, 54, 54, 53, 102, 100, 49, 53, 51, 97, 97, 49,
				54, 100, 102, 55, 52, 48, 102, 50, 53, 51, 55, 50, 55, 56, 102, 0, 97, 49, 57, 49,
				101, 101, 57, 54, 48, 52, 56, 98, 102, 56, 57, 57, 55, 51, 52, 57, 97, 48, 49, 55,
				53, 56, 101, 52, 98, 55, 97, 0, 50, 100, 53, 57, 102, 53, 52, 53, 51, 56, 57, 56,
				101, 98, 98, 49, 100, 50, 51, 98, 102, 52, 53, 57, 101, 54, 54, 55, 97, 54, 51, 0,
				52, 98, 49, 54, 99, 102, 52, 50, 50, 102, 99, 57, 51, 53, 51, 100, 52, 98, 56, 98,
				98, 54, 48, 56, 98, 53, 57, 50, 48, 101, 52, 0, 53, 55, 51, 102, 51, 53, 102, 48,
				55, 48, 55, 100, 51, 50, 56, 97, 102, 97, 52, 56, 50, 49, 102, 99, 101, 102, 49,
				54, 52, 57, 102, 0, 57, 100, 53, 50, 101, 55, 98, 53, 55, 50, 53, 101, 51, 48, 57,
				53, 100, 56, 101, 101, 101, 97, 53, 100, 54, 99, 50, 53, 51, 56, 48, 0, 100, 52,
				101, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
				0, 0, 0, 0, 52, 96, 128, 190, 131, 244, 138, 209, 116, 140, 74, 211, 57, 171, 220,
				184, 3, 54, 142, 253, 209, 246, 86, 137, 97, 159, 248, 194, 8, 117, 93, 0, 132,
				238, 252, 248, 55, 182, 28, 71, 155, 51, 50, 5, 155, 200, 232, 155, 73, 10, 157,
				80, 43, 174, 202, 237, 68, 132, 51, 212, 225, 97, 113, 0, 0, 167, 28, 187, 26, 3,
				135, 89, 142, 80, 157, 159, 202, 181, 17, 2, 47, 67, 123, 12, 175, 19, 89, 19, 21,
				195, 241, 187, 240, 79, 24, 0, 157, 131, 240, 20, 128, 98, 16, 218, 110, 225, 210,
				248, 12, 240, 249, 192, 143, 29, 19, 43, 224, 66, 118, 144, 21, 246, 23, 79, 210,
				178, 76, 0,
			]),
			None,
		]
		.to_vec();

		let block = BlockVerified {
			header_hash: hex!("5bc959e1d05c68f7e1b5bc3a83cfba4efe636ce7f86102c30bcd6a2794e75afe")
				.into(),
			block_num: 288,
			dimensions,
			lookup: AppDataIndex {
				size: 12,
				index: [(1, 1)].to_vec(),
			},
			commitments: [
				[
					165, 227, 207, 130, 59, 77, 78, 242, 184, 232, 114, 218, 145, 167, 149, 53, 89,
					7, 230, 49, 85, 113, 218, 116, 43, 195, 144, 203, 149, 114, 106, 89, 73, 164,
					17, 163, 3, 145, 173, 6, 119, 222, 17, 60, 251, 215, 40, 192,
				],
				[
					165, 227, 207, 130, 59, 77, 78, 242, 184, 232, 114, 218, 145, 167, 149, 53, 89,
					7, 230, 49, 85, 113, 218, 116, 43, 195, 144, 203, 149, 114, 106, 89, 73, 164,
					17, 163, 3, 145, 173, 6, 119, 222, 17, 60, 251, 215, 40, 192,
				],
			]
			.to_vec(),
		};
		mock_client
			.expect_fetch_rows_from_dht()
			.returning(move |_, _, _| {
				let dht_rows_clone = dht_rows.clone();
				Box::pin(async move { dht_rows_clone })
			});
		if cfg.disable_rpc {
			mock_client.expect_get_kate_rows().never();
		} else {
			mock_client.expect_get_kate_rows().returning(move |_, _| {
				let kate_rows_clone = kate_rows.clone();
				Box::pin(async move { Ok(kate_rows_clone) })
			});
		}
		mock_client
			.expect_reconstruct_rows_from_dht()
			.returning(|_, _, _, _, _| Box::pin(async move { Ok(vec![]) }));
		mock_client
			.expect_store_encoded_data_in_db()
			.returning(|_, _, _: &Vec<Vec<u8>>| Ok(()));
		process_block(mock_client, &cfg, 1, &block, pp)
			.await
			.unwrap();
	}
}
