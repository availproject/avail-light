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

	async fn process_block(
		&self,
		cfg: &AppClientConfig,
		app_id: u32,
		block: &BlockVerified,
		pp: PublicParameters,
	) -> Result<()>;

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

	async fn process_block(
		&self,
		cfg: &AppClientConfig,
		app_id: u32,
		block: &BlockVerified,
		pp: PublicParameters,
	) -> Result<()> {
		process_block(&self, &cfg, app_id, &block, pp).await?;
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
async fn process_block(
	app_client: &AppClientImpl,
	cfg: &AppClientConfig,
	// db: Arc<DB>,
	// network_client: Client,
	// rpc_client: &OnlineClient<AvailConfig>,
	app_id: u32,
	block: &BlockVerified,
	pp: PublicParameters,
) -> Result<()> {
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
		info!("kate rows {:?} {:?}", dht_missing_rows, block.header_hash);
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
	info!("dht rows {:?}", dht_rows);
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

	info!("data is {:?}", data);
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
		if let Err(error) = process_block(&app_client, &cfg, app_id, &block, pp.clone()).await {
			error!(block_number, "Cannot process block: {error}");
		} else {
			debug!(block_number, "Block processed");
		}
	}
}

#[cfg(test)]
mod tests {
	use crate::types::RuntimeConfig;
	use kate_recovery::{index::AppDataIndex, testnet};

	use super::*;
	use hex_literal::hex;

	#[tokio::test]
	async fn test_process_blocks() {
		let cfg = AppClientConfig::from(&RuntimeConfig::default());
		let pp = testnet::public_params(1024);
		let mut mock_client = MockAppClient::new();
		let dimensions = Dimensions::new(1, 128).unwrap();
		let block: BlockVerified = BlockVerified {
			header_hash: hex!("c7363a77d5353fe89090b7169549e5553049732db5adc826198ba05f4aa3cc64")
				.into(),
			block_num: 14,
			dimensions,
			lookup: AppDataIndex {
				size: 70,
				index: [(1, 1)].to_vec(),
			},
			commitments: [
				[
					176, 69, 199, 87, 112, 184, 224, 59, 102, 59, 244, 146, 54, 104, 209, 32, 43,
					244, 35, 9, 177, 42, 176, 215, 139, 133, 103, 118, 138, 53, 44, 1, 176, 169,
					98, 20, 79, 246, 30, 194, 117, 159, 230, 51, 77, 247, 95, 104,
				],
				[
					176, 69, 199, 87, 112, 184, 224, 59, 102, 59, 244, 146, 54, 104, 209, 32, 43,
					244, 35, 9, 177, 42, 176, 215, 139, 133, 103, 118, 138, 53, 44, 1, 176, 169,
					98, 20, 79, 246, 30, 194, 117, 159, 230, 51, 77, 247, 95, 104,
				],
			]
			.to_vec(),
		};
		mock_client
			.expect_process_block()
			.returning(|_, _, _, _| Box::pin(async move { Ok(()) }));
		mock_client
			.process_block(&cfg, 1, &block, pp)
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn test_reconstructed_from_dht() {
		let pp = testnet::public_params(1024);
		let dimensions = Dimensions::new(1, 128).unwrap();
		let commitments: Vec<[u8; 48]> = [
			[
				149, 184, 109, 158, 93, 47, 109, 200, 158, 234, 243, 85, 16, 251, 61, 140, 9, 171,
				131, 132, 45, 221, 19, 54, 12, 237, 227, 155, 110, 97, 233, 55, 140, 208, 53, 132,
				252, 239, 173, 221, 203, 8, 67, 164, 172, 16, 72, 124,
			],
			[
				149, 184, 109, 158, 93, 47, 109, 200, 158, 234, 243, 85, 16, 251, 61, 140, 9, 171,
				131, 132, 45, 221, 19, 54, 12, 237, 227, 155, 110, 97, 233, 55, 140, 208, 53, 132,
				252, 239, 173, 221, 203, 8, 67, 164, 172, 16, 72, 124,
			],
		]
		.to_vec();
		let missing_rows: Vec<u32> = vec![];
		let mut mock_client = MockAppClient::new();
		mock_client
			.expect_reconstruct_rows_from_dht()
			.returning(|_, _, _, _, _| Box::pin(async move { Ok(vec![]) }));
		mock_client
			.reconstruct_rows_from_dht(pp, 2, &dimensions, &commitments, &missing_rows)
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn test_kate_rows() {
		let cfg = AppClientConfig::from(&RuntimeConfig::default());
		let dht_missing_rows: Vec<u32> = vec![0];
		let header_hash: H256 =
			hex!("202fbd0018c959347009d51cd7d8849ff3b3d2a22436fabe09bfbd4ad74e6fd3").into();
		let mut mock_client = MockAppClient::new();
		let kate_rows: Vec<Option<Vec<u8>>> = [
			Some(
				[
					4, 44, 40, 4, 3, 0, 11, 1, 188, 158, 178, 135, 1, 128, 0, 0, 0, 0, 0, 0, 0, 0,
					0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 217, 4, 209, 4, 132, 0, 212, 53, 147, 199, 21,
					253, 211, 28, 97, 20, 26, 189, 4, 169, 159, 214, 130, 44, 133, 88, 133, 76,
					205, 227, 0, 154, 86, 132, 231, 165, 109, 162, 125, 1, 44, 249, 132, 221, 192,
					78, 123, 66, 163, 21, 194, 52, 14, 60, 188, 0, 61, 84, 114, 249, 229, 36, 0,
					72, 161, 99, 24, 6, 214, 251, 75, 19, 80, 185, 225, 250, 191, 195, 234, 53, 5,
					105, 62, 1, 186, 114, 220, 61, 242, 98, 23, 157, 20, 169, 0, 24, 143, 9, 222,
					166, 235, 210, 163, 247, 218, 141, 180, 0, 212, 0, 4, 29, 1, 33, 3, 56, 56, 53,
					49, 99, 52, 99, 53, 51, 50, 101, 0, 102, 99, 101, 50, 101, 55, 48, 54, 50, 101,
					97, 98, 97, 100, 55, 97, 100, 52, 51, 56, 50, 56, 100, 52, 52, 52, 49, 98, 100,
					52, 52, 0, 99, 48, 52, 55, 97, 49, 100, 57, 51, 97, 97, 100, 51, 48, 48, 99,
					56, 51, 51, 48, 55, 56, 98, 55, 53, 56, 55, 57, 52, 48, 54, 0, 101, 54, 98, 49,
					55, 98, 99, 101, 97, 51, 50, 53, 52, 99, 57, 48, 56, 55, 54, 49, 51, 50, 53,
					98, 56, 102, 56, 48, 49, 99, 101, 0, 57, 100, 98, 102, 52, 101, 54, 101, 51,
					48, 102, 50, 97, 51, 51, 101, 54, 97, 57, 50, 52, 57, 98, 48, 101, 97, 102, 98,
					56, 52, 102, 0, 98, 49, 98, 50, 49, 57, 57, 102, 97, 49, 53, 48, 52, 48, 57,
					48, 57, 100, 99, 49, 53, 97, 102, 55, 101, 57, 100, 97, 100, 97, 55, 0, 57, 57,
					53, 50, 56, 56, 98, 102, 54, 53, 97, 57, 102, 99, 52, 51, 101, 54, 56, 101, 99,
					56, 50, 50, 57, 98, 98, 98, 101, 102, 56, 0, 102, 54, 49, 128, 0, 0, 0, 0, 0,
					0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 52, 96,
					128, 190, 131, 244, 138, 209, 116, 140, 74, 211, 57, 171, 220, 184, 3, 54, 142,
					253, 209, 246, 86, 137, 97, 159, 248, 194, 8, 117, 93, 0, 132, 238, 252, 248,
					55, 182, 28, 71, 155, 51, 50, 5, 155, 200, 232, 155, 73, 10, 157, 80, 43, 174,
					202, 237, 68, 132, 51, 212, 225, 97, 113, 0, 0, 167, 28, 187, 26, 3, 135, 89,
					142, 80, 157, 159, 202, 181, 17, 2, 47, 67, 123, 12, 175, 19, 89, 19, 21, 195,
					241, 187, 240, 79, 24, 0, 157, 131, 240, 20, 128, 98, 16, 218, 110, 225, 210,
					248, 12, 240, 249, 192, 143, 29, 19, 43, 224, 66, 118, 144, 21, 246, 23, 79,
					210, 178, 76, 0,
				]
				.to_vec(),
			),
			None,
		]
		.to_vec();
		if cfg.disable_rpc {
			mock_client.expect_get_kate_rows().never();
		} else {
			mock_client.expect_get_kate_rows().returning(move |_, _| {
				let kate_rows_clone = kate_rows.clone();
				Box::pin(async move { Ok(kate_rows_clone) })
			});
			mock_client
				.get_kate_rows(dht_missing_rows, header_hash)
				.await
				.unwrap();
		}
	}

	#[test]
	fn test_store_data_in_db() {
		let data: Vec<Vec<u8>> = [vec![
			209, 4, 132, 0, 212, 53, 147, 199, 21, 253, 211, 28, 97, 20, 26, 189, 4, 169, 159, 214,
			130, 44, 133, 88, 133, 76, 205, 227, 154, 86, 132, 231, 165, 109, 162, 125, 1, 50, 250,
			181, 208, 222, 28, 133, 156, 82, 243, 96, 204, 164, 132, 142, 96, 124, 146, 222, 23,
			125, 67, 157, 133, 150, 1, 228, 125, 227, 0, 247, 24, 56, 226, 216, 196, 109, 196, 8,
			155, 143, 35, 28, 33, 96, 180, 211, 48, 151, 53, 204, 119, 46, 199, 120, 37, 244, 183,
			177, 253, 89, 236, 9, 135, 180, 0, 216, 0, 4, 29, 1, 33, 3, 48, 50, 98, 50, 98, 52, 51,
			56, 97, 57, 50, 101, 101, 97, 48, 98, 100, 56, 97, 55, 99, 102, 52, 99, 51, 99, 50,
			101, 99, 52, 98, 102, 100, 57, 51, 102, 48, 56, 56, 48, 102, 101, 100, 52, 56, 56, 49,
			102, 55, 102, 56, 49, 53, 55, 56, 52, 100, 50, 49, 48, 53, 100, 102, 56, 55, 97, 56,
			97, 57, 99, 100, 54, 53, 57, 55, 50, 48, 55, 55, 54, 97, 102, 54, 51, 100, 48, 51, 98,
			100, 102, 51, 97, 57, 99, 101, 56, 98, 51, 52, 50, 102, 52, 57, 54, 49, 98, 56, 101,
			49, 55, 53, 102, 49, 97, 53, 97, 101, 97, 49, 50, 102, 52, 98, 99, 54, 51, 97, 101, 49,
			99, 57, 56, 101, 97, 54, 55, 99, 57, 101, 100, 100, 55, 55, 102, 97, 49, 100, 57, 48,
			52, 100, 102, 51, 102, 101, 49, 57, 99, 102, 97, 49, 48, 48, 48, 53, 100, 102, 53, 51,
			57, 54, 102, 54, 54, 101, 97, 57, 52, 51, 53, 99, 55, 53, 50, 56, 98, 100, 98, 100, 52,
			54, 100, 51, 98, 100, 102, 53, 97, 101, 57,
		]]
		.to_vec();
		let mut mock_client = MockAppClient::new();
		mock_client
			.expect_store_encoded_data_in_db()
			.withf(|x, y, _| *x == 1 && *y == 2)
			.returning(|_, _, _: &Vec<Vec<u8>>| Ok(()));
		mock_client.store_encoded_data_in_db(1, 2, &data).unwrap();
	}
}
