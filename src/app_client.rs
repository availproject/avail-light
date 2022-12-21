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
use avail_subxt::AvailConfig;
use dusk_plonk::commitment_scheme::kzg10::PublicParameters;
use kate_recovery::{
	com::{columns_positions, decode_app_extrinsics, reconstruct_columns},
	commitments,
	config::{self, CHUNK_SIZE},
	data::{Cell, DataCell},
	matrix::{Dimensions, Position},
};
use rocksdb::DB;
use std::{
	collections::HashMap,
	sync::{mpsc::Receiver, Arc},
};
use subxt::OnlineClient;
use tracing::{debug, error, info, instrument};

use crate::{
	data::store_encoded_data_in_db,
	network::Client,
	proof,
	rpc::get_kate_app_data,
	types::{AppClientConfig, ClientMsg},
};

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
		.await
		.context("Failed to fetch cells from DHT")?;

	let (verified, mut unverified) =
		proof::verify(block_number, dimensions, &fetched, &commitments, pp)
			.context("Failed to verify fetched cells")?;

	fetched.retain(|cell| verified.contains(&cell.position));
	unfetched.append(&mut unverified);

	Ok((fetched, unfetched))
}

async fn fetch_and_verify_rows(
	pp: PublicParameters,
	network_client: Client,
	block_number: u32,
	dimensions: &Dimensions,
	commitments: &[[u8; config::COMMITMENT_SIZE]],
	missing_rows: &[u32],
) -> Result<Vec<(u32, Vec<u8>)>> {
	let missing_cells = dimensions.extended_rows_positions(missing_rows);

	if missing_cells.is_empty() {
		return Ok(vec![]);
	}

	info!(
		block_number,
		"Fetching missing {} rows cells from DHT...",
		missing_cells.len()
	);

	let (fetched, unfetched) = fetch_verified(
		&pp,
		&network_client,
		block_number,
		dimensions,
		commitments,
		&missing_cells,
	)
	.await?;

	info!(
		block_number,
		"Failed to fetch or verify {} missing cells",
		unfetched.len()
	);

	let missing_cells = columns_positions(dimensions, &unfetched, 0.66);

	let (missing_fetched, _) = fetch_verified(
		&pp,
		&network_client,
		block_number,
		dimensions,
		commitments,
		&missing_cells,
	)
	.await?;

	let reconstructed = reconstruct_columns(dimensions, &missing_fetched)?;

	debug!(
		block_number,
		"Reconstructed {} columns",
		reconstructed.keys().len()
	);

	let mut reconstructed_cells = unfetched
		.into_iter()
		.map(|position| data_cell(position, &reconstructed))
		.collect::<Result<Vec<_>>>()?;

	info!(
		block_number,
		"Reconstructed {} missing cells",
		reconstructed_cells.len()
	);

	let mut data_cells: Vec<DataCell> = fetched.into_iter().map(Into::into).collect::<Vec<_>>();

	data_cells.append(&mut reconstructed_cells);

	data_cells
		.sort_by(|a, b| (a.position.row, a.position.col).cmp(&(b.position.row, b.position.col)));

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

#[instrument(skip_all, fields(block = block.block_num), level = "trace")]
async fn process_block(
	cfg: &AppClientConfig,
	db: Arc<DB>,
	network_client: Client,
	rpc_client: &OnlineClient<AvailConfig>,
	app_id: u32,
	block: &ClientMsg,
	pp: PublicParameters,
) -> Result<()> {
	let lookup = &block.lookup;
	let block_number = block.block_num;
	let dimensions = &block.dimensions;
	let commitments = &block.commitments;

	let mut rows = get_kate_app_data(rpc_client, block.header_hash, app_id).await?;

	let rows_count = rows.iter().filter(|row| row.is_some()).count();
	info!(block_number, "Found {rows_count} rows for app {app_id}");

	let (verified_rows, missing_rows) =
		commitments::verify_equality(&pp, commitments, &rows, lookup, dimensions, app_id)?;

	info!(block_number, "Verified rows: {verified_rows:?}");
	debug!(block_number, "{} rows are verified", verified_rows.len());
	debug!(block_number, "{} rows are missing", missing_rows.len());

	if missing_rows.len() * dimensions.cols() as usize > cfg.threshold {
		return Err(anyhow::anyhow!("Too many cells are missing"));
	}

	let dht_rows = fetch_and_verify_rows(
		pp,
		network_client,
		block_number,
		dimensions,
		commitments,
		&missing_rows,
	)
	.await?;

	for (index, row) in dht_rows {
		let i: usize = index.try_into()?;
		rows[i] = Some(row);
	}

	let data_cells =
		data_cells_from_rows(rows).context("Failed to create data cells from rows got from RPC")?;

	let data = decode_app_extrinsics(lookup, dimensions, data_cells, app_id)
		.context("Failed to decode app extrinsics")?;

	store_encoded_data_in_db(db, app_id, block_number, &data)
		.context("Failed to store data into database")?;

	let bytes_count = data.iter().fold(0usize, |acc, x| acc + x.len());
	info!(block_number, "Stored {bytes_count} bytes into database");

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
	block_receive: Receiver<ClientMsg>,
	pp: PublicParameters,
) {
	info!("Starting for app {app_id}...");

	for block in block_receive {
		let block_number = block.block_num;

		info!(block_number, "Block available");

		if block.dimensions.cols() == 0 {
			continue;
		}

		if block
			.lookup
			.index
			.iter()
			.filter(|&(id, _)| id == &app_id)
			.count() == 0
		{
			info!(block_number, "No cells for app {app_id}");
			continue;
		}

		if let Err(error) = process_block(
			&cfg,
			db.clone(),
			network_client.clone(),
			&rpc_client,
			app_id,
			&block,
			pp.clone(),
		)
		.await
		{
			error!(block_number, "Cannot process block: {error}");
		}
	}
}
