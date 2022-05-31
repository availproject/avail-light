extern crate anyhow;
extern crate ipfs_embed;
extern crate libipld;

use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context, Result};
use futures::future::join_all;
use ipfs_embed::{Cid, DefaultParams, Ipfs, Key, TempPin};
use libipld::{
	codec_impl::IpldCodec,
	multihash::{Code, MultihashDigest},
	Ipld,
};

use crate::{
	rpc,
	types::{BaseCell, Cell as DataCell, DataMatrix, IpldBlock, L0Col, L1Row},
};

fn construct_cell(
	row: u16,
	col: u16,
	row_count: u16,
	cells: Arc<Vec<Option<Vec<u8>>>>,
) -> Result<BaseCell, String> {
	let cell_index = (col as usize) * (row_count as usize) + (row as usize);
	cells[cell_index]
		.as_ref()
		.ok_or_else(|| "failed to construct cell due to unavailability of data".to_owned())
		.and_then(|cell| {
			let data = Ipld::Bytes(cell.to_owned());
			IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &data)
				.map_err(|_| "failed to IPLD encode cell of data matrix".to_owned())
		})
}

fn construct_colwise(
	row_count: u16,
	col: u16,
	cells: Arc<Vec<Option<Vec<u8>>>>,
) -> Result<L0Col, String> {
	let mut base_cells: Vec<BaseCell> = Vec::with_capacity(row_count as usize);

	for row in 0..row_count {
		match construct_cell(row, col, row_count, cells.clone()) {
			Ok(cell) => {
				base_cells.push(cell);
			},
			Err(msg) => return Err(msg),
		};
	}

	Ok(L0Col { base_cells })
}

pub fn construct_matrix(
	block: u64,
	row_count: u16,
	col_count: u16,
	cells: Arc<Vec<Option<Vec<u8>>>>,
) -> Result<DataMatrix, String> {
	(0..col_count)
		.map(move |col| construct_colwise(row_count, col, cells.clone()))
		.collect::<Result<Vec<_>, String>>()
		.map(|l0_cols| DataMatrix {
			l1_row: L1Row { l0_cols },
			block_num: block as i128,
		})
}

type Cell = Vec<u8>;
type Column = Vec<Option<Cell>>;

#[derive(Debug)]
pub struct Matrix {
	columns: Vec<Column>,
}

impl Matrix {
	pub fn new() -> Self { Matrix { columns: vec![] } }

	pub fn columns(&self) -> impl Iterator<Item = &Column> { self.columns.iter() }

	pub fn get(&self, col: usize, row: usize) -> Option<&Option<Cell>> {
		self.columns.get(col).and_then(|col| col.get(row))
	}
}

pub fn matrix_cells(rows: u16, cols: u16) -> impl Iterator<Item = (usize, usize)> {
	(0..cols as usize).flat_map(move |col| (0..rows as usize).map(move |row| (row, col)))
}

pub fn empty_cells(matrix: &Matrix, cols: u16, rows: u16) -> Vec<(usize, usize)> {
	// TODO: Optimal solution is to use proper Matrix abstraction to derive empty cells
	matrix_cells(rows, cols)
		.filter(|(row, col)| {
			matrix
				.get(*col, *row)
				.map(|cell| cell.is_none())
				.unwrap_or(true)
		})
		.collect::<Vec<(usize, usize)>>()
}

pub fn non_empty_cells_len(matrix: &Matrix) -> usize {
	matrix
		.columns()
		.fold(0usize, |sum, val| sum + val.iter().flatten().count())
}

async fn get_cell(ipfs: &Ipfs<DefaultParams>, cid: Cid) -> Result<Option<Cell>> {
	let peers = ipfs.peers();
	ipfs.fetch(&cid, peers)
		.await
		.and_then(|result| result.ipld())
		.map(|decoded| extract_cell(&decoded))
		.context("Cannot get cell")
}

async fn get_column(ipfs: &Ipfs<DefaultParams>, cid: Cid) -> Result<Column> {
	let peers = ipfs.peers();
	let links = ipfs
		.fetch(&cid, peers)
		.await
		.and_then(|result| result.ipld())
		.and_then(|decoded| extract_links(&decoded).context("Cannot extract cell links"))
		.context("Cannot get cell links")?;

	let future_col = links
		.iter()
		.flat_map(|link| link.context("Cell link is missing"))
		.map(|cell_cid| get_cell(ipfs, cell_cid))
		.collect::<Vec<_>>();

	let col = join_all(future_col).await;
	col.into_iter()
		.collect::<Result<Vec<_>>>()
		.context("Cannot get column cells")
}

pub async fn get_matrix(ipfs: &Ipfs<DefaultParams>, root_cid: Option<Cid>) -> Result<Matrix> {
	match root_cid {
		None => Ok(Matrix::new()),
		Some(cid) => {
			let peers = ipfs.peers();
			let column_cids = ipfs
				.fetch(&cid, peers)
				.await
				.and_then(|result| result.ipld())
				.and_then(|root| destructure_matrix(&root).context("Cannot destructure root block"))
				.and_then(|(_, column_cids, _)| column_cids.context("No column block cids"))
				.context("Cannot get column cids")?;

			let future_mat = column_cids
				.iter()
				.flat_map(|column_cid| column_cid.context("No column block cid"))
				.map(|column_cid| get_column(ipfs, column_cid))
				.collect::<Vec<_>>();

			let mat = join_all(future_mat).await;

			mat.into_iter()
				.collect::<Result<Vec<Column>>>()
				.map(|columns| Matrix { columns })
				.context("Cannot get matrix")
		},
	}
}

async fn push_cell(
	cell: BaseCell,
	ipfs: &Ipfs<DefaultParams>,
	pin: &mut TempPin,
) -> anyhow::Result<Cid, String> {
	let cid = cell.cid().clone();
	match ipfs.temp_pin(pin, &cid) {
		Ok(_) => match ipfs.insert(cell) {
			Ok(_) => Ok(cid),
			Err(_) => Err("failed to IPFS insert cell of data matrix".to_owned()),
		},
		Err(_) => Err("failed to IPFS pin cell of data matrix".to_owned()),
	}
}

async fn push_col(
	col: L0Col,
	ipfs: &Ipfs<DefaultParams>,
	pin: &mut TempPin,
) -> anyhow::Result<Cid, String> {
	let mut cell_cids: Vec<Ipld> = Vec::with_capacity(col.base_cells.len());

	for cell in col.base_cells {
		match push_cell(cell, ipfs, pin).await {
			Ok(cid) => {
				cell_cids.push(Ipld::Link(cid));
			},
			Err(msg) => return Err(msg),
		};
	}

	let col = Ipld::List(cell_cids);
	match IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &col) {
		Ok(coded_col) => {
			let cid = coded_col.cid().clone();
			match ipfs.temp_pin(pin, &cid) {
				Ok(_) => match ipfs.insert(coded_col) {
					Ok(_) => Ok(cid),
					Err(_) => Err("failed to IPFS insert column of data matrix".to_owned()),
				},
				Err(_) => Err("failed to IPFS pin column of data matrix".to_owned()),
			}
		},
		Err(_) => Err("failed to IPLD encode column of data matrix".to_owned()),
	}
}

async fn push_row(
	row: L1Row,
	block_num: i128,
	latest_cid: Option<Cid>,
	ipfs: &Ipfs<DefaultParams>,
	pin: &mut TempPin,
) -> anyhow::Result<Cid, String> {
	let mut col_cids: Vec<Ipld> = Vec::with_capacity(row.l0_cols.len());

	for col in row.l0_cols {
		match push_col(col, ipfs, pin).await {
			Ok(cid) => {
				col_cids.push(Ipld::Link(cid));
			},
			Err(msg) => {
				return Err(msg);
			},
		};
	}

	let mut map = BTreeMap::new();

	map.insert("columns".to_owned(), Ipld::List(col_cids));
	map.insert("block".to_owned(), Ipld::Integer(block_num));
	map.insert("prev".to_owned(), match latest_cid {
		Some(cid) => Ipld::Link(cid),
		None => Ipld::Null,
	});

	let map = Ipld::StringMap(map);
	match IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &map) {
		Ok(coded_mat) => {
			let cid = coded_mat.cid().clone();
			match ipfs.temp_pin(pin, &cid) {
				Ok(_) => match ipfs.insert(coded_mat) {
					Ok(_) => Ok(cid),
					Err(_) => Err("failed to IPFS insert row of data matrix".to_owned()),
				},
				Err(_) => Err("failed to IPFS pin row of data matrix".to_owned()),
			}
		},
		Err(_) => Err("failed to IPLD encode row of data matrix".to_owned()),
	}
}

pub async fn push_matrix(
	data_matrix: DataMatrix,
	latest_cid: Option<Cid>,
	ipfs: &Ipfs<DefaultParams>,
	pin: &mut TempPin,
) -> anyhow::Result<Cid, String> {
	push_row(
		data_matrix.l1_row,
		data_matrix.block_num,
		latest_cid,
		ipfs,
		pin,
	)
	.await
}

/// Extracts respective CID from IPLD encapsulated data object
pub fn extract_cid(data: &Ipld) -> Option<Cid> {
	match data {
		Ipld::Link(cid) => Some(*cid),
		Ipld::Null => None,
		_ => None,
	}
}

/// Extracts block number from IPLD encapsulated data object
pub fn extract_block(data: &Ipld) -> Option<i128> {
	match data {
		Ipld::Integer(block) => Some(*block),
		_ => None,
	}
}

/// Extracts out list of CIDs from IPLD coded message
pub fn extract_links(data: &Ipld) -> Option<Vec<Option<Cid>>> {
	match data {
		Ipld::List(links) => Some(links.iter().map(extract_cid).collect()),
		_ => None,
	}
}

/// Extracts out content of cell from IPLD coded message
pub fn extract_cell(data: &Ipld) -> Option<Vec<u8>> {
	match data {
		Ipld::Bytes(v) => Some(v.to_vec()),
		_ => None,
	}
}

/// Given a decoded IPLD object, which represented one coded data matrix
/// extracts out all components ( i.e. block number, column CID list
/// and previous CID )
pub fn destructure_matrix(
	data: &Ipld,
) -> Option<(Option<i128>, Option<Vec<Option<Cid>>>, Option<Cid>)> {
	match data {
		Ipld::StringMap(map) => {
			let block = map.get("block");
			let cols = map.get("columns");
			let prev = map.get("prev");

			match (block, cols, prev) {
				(Some(block), Some(cols), Some(prev)) => {
					Some((extract_block(block), extract_links(cols), extract_cid(prev)))
				},
				_ => None,
			}
		},
		_ => None,
	}
}

// Takes block number along with respective CID of block data matrix
// which was just inserted into local data store ( IPFS backed )
// and encodes it which will be returned back from this function
// as byte array ( well vector ). This byte array will be published
// on some pre-agreed upon topic over Gossipsub network, so that
// if some other peer doesn't compute/ store this block data matrix itself,
// it should be able to atleast learn of the CID corresponding to block number,
// so that it can fetch it when required.
pub fn prepare_block_cid_fact_message(block: i128, cid: Cid) -> Result<Vec<u8>, String> {
	let mut map = BTreeMap::new();

	map.insert("block".to_owned(), Ipld::Integer(block));
	map.insert("cid".to_owned(), Ipld::Link(cid));

	let map = Ipld::StringMap(map);
	match IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &map) {
		Ok(coded) => Ok(coded.data().to_vec()),
		Err(_) => Err("failed to IPLD encode fact topic gossip message".to_owned()),
	}
}

// Takes a IPLD coded fact channel message as byte array, which is
// received from other peer on fact topic, and attempts
// to decode it into two constituent components i.e. block number
// and respective Cid of block data matrix
pub fn decode_block_cid_fact_message(msg: Vec<u8>) -> Option<(i128, Cid)> {
	let m_hash = Code::Blake3_256.digest(&msg[..]);
	let cid = Cid::new_v1(IpldCodec::DagCbor.into(), m_hash);

	let coded_msg: IpldBlock;
	if let Ok(v) = IpldBlock::new(cid, msg) {
		coded_msg = v;
	} else {
		return None;
	}

	let decoded_msg: Ipld;
	if let Ok(v) = coded_msg.ipld() {
		decoded_msg = v;
	} else {
		return None;
	}

	match decoded_msg {
		Ipld::StringMap(map) => {
			let map: BTreeMap<String, Ipld> = map;
			let block = if let Some(data) = map.get("block") {
				extract_block(data)
			} else {
				None
			};
			let cid = if let Some(data) = map.get("cid") {
				extract_cid(data)
			} else {
				None
			};
			if block == None || cid == None {
				None
			} else {
				Some((block.unwrap(), cid.unwrap()))
			}
		},
		_ => None,
	}
}

// Some peer who doesn't know about block data matrix CID of some specified
// block number, may need to know same, sends a message over pre-agreed upon
// channel ( topic ). I call this channel ask channel, where peers get to ask
// questions & expect someone will answer it
//
// Same channel will be used when attempting to answer back to question
pub fn prepare_block_cid_ask_message(block: i128, cid: Option<Cid>) -> Result<Vec<u8>, String> {
	let mut map = BTreeMap::new();

	map.insert("block".to_owned(), Ipld::Integer(block));
	map.insert("cid".to_owned(), match cid {
		Some(cid) => Ipld::Link(cid),
		None => Ipld::Null,
	});

	let map = Ipld::StringMap(map);

	match IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &map) {
		Ok(coded) => Ok(coded.data().to_vec()),
		Err(_) => Err("failed to IPLD encode ask topic gossip message".to_owned()),
	}
}

// Accepts IPLD coded ask channel message and attempts to decode it
// such that whether this message is a question or answer to it
// can be understood by function invoker by checking returned component types
//
// Always block number should be coded inside message, but if this is a question kind message
// Cid will be Null encoded ( returned as None ), where for answer type message
// Cid will be encoded as IPLD Link ( returned as Some ).
pub fn decode_block_cid_ask_message(msg: Vec<u8>) -> Option<(i128, Option<Cid>)> {
	let m_hash = Code::Blake3_256.digest(&msg[..]);
	let cid = Cid::new_v1(IpldCodec::DagCbor.into(), m_hash);

	let coded_msg: IpldBlock;
	if let Ok(v) = IpldBlock::new(cid, msg) {
		coded_msg = v;
	} else {
		return None;
	}

	let decoded_msg: Ipld;
	if let Ok(v) = coded_msg.ipld() {
		decoded_msg = v;
	} else {
		return None;
	}

	match decoded_msg {
		Ipld::StringMap(map) => {
			let map: BTreeMap<String, Ipld> = map;
			let block = if let Some(data) = map.get("block") {
				extract_block(data)
			} else {
				None
			};
			let cid = if let Some(data) = map.get("cid") {
				extract_cid(data)
			} else {
				None
			};
			if block == None {
				None
			} else {
				Some((block.unwrap(), cid))
			}
		},
		_ => None,
	}
}

pub async fn ipfs_priority_get_cells(
	cells: &mut Vec<DataCell>,
	ipfs: &Ipfs<DefaultParams>,
	rpc_url: &String,
	block_num: u64,
) -> Result<()> {
	// Try fetching the cells from IPFS first
	for req_cell_data in cells.iter_mut() {
		let peers = ipfs.peers();
		log::info!("Number of known peers: {}", peers.len());
		if peers.len() == 0 {
			break;
		}
		// TODO: paralelize fetching
		// Skip cells that return error when fetching from IPFS
		log::info!(
			"Requesting from IPFS: cell reference: {}.",
			req_cell_data.reference()
		);

		let key = Key::from(req_cell_data.reference().as_bytes().to_vec());
		let (block_cid, get_record_error): (Cid, _) = ipfs
			.get_record(key, ipfs_embed::Quorum::One)
			.await
			.map(|record| record[0].record.value.to_vec())
			.and_then(|cid_value| Cid::try_from(cid_value).context("Invalid CID value"))
			.map(|cid| (cid, None))
			.unwrap_or_else(|error| (Cid::default(), Some(error)));

		if let Some(error) = get_record_error {
			log::error!(
				"Cannot get CID from record: {}. Cell reference: {}",
				error,
				req_cell_data.reference()
			);
			continue;
		}

		if let Err(error) = ipfs.fetch(&block_cid, peers).await.and_then(|block| {
			// IPFS block data contains cell proof
			req_cell_data.proof = block.data().to_vec();
			req_cell_data.data = req_cell_data.proof[48..].to_vec();
			Ok(())
		}) {
			log::error!(
				"Error fetching data from IPFS: {}. Cell reference: {}",
				error,
				req_cell_data.reference()
			);
			continue;
		}
	}
	let (fetched, unfetched): (Vec<_>, Vec<_>) = cells
		.iter()
		.cloned()
		.partition(|cell| !cell.proof.is_empty());

	log::info!("Number of cells fetched from IPFS: {}", fetched.len());

	// Retrieve remaining cell proofs via RPC call to node
	// TODO: handle case if 1 or more cells are unavailable via RPC (retry mechanism, generate new cells, etc)
	let mut remaining_cells = rpc::get_kate_proof(&rpc_url, block_num, unfetched).await?;
	remaining_cells.iter_mut().for_each(move |cell| {
		cell.data = cell.proof[48..].to_vec();
	});

	for cell_data in cells.iter_mut() {
		// Skip cells with data retrieved from IPFS
		if cell_data.data.len() != 0 {
			continue;
		}
		// Complexity not an issue bcs of the small number of remaining cells
		for remaining_cell in remaining_cells.iter().filter(|cell| {
			cell_data.block == cell.block && cell_data.col == cell.col && cell_data.row == cell.row
		}) {
			cell_data.data = remaining_cell.data.clone();
			cell_data.proof = remaining_cell.proof.clone();
		}
	}

	return Ok(());
}

#[cfg(test)]
mod tests {
	extern crate rand;

	use ipfs_embed::{Multiaddr, PeerId};
	use proptest::{collection, prelude::*};
	use rand::prelude::random;
	use test_case::test_case;

	use super::{super::client::make_client, construct_matrix, *};

	fn random_data(n: u64) -> Vec<u8> {
		let mut data = Vec::with_capacity(n as usize);
		for _ in 0..n {
			data.push(random::<u8>());
		}
		data
	}

	#[test]
	fn fact_message_encoding_decoding_success() {
		let block: i128 = 1 << 31;
		let cid: Cid = {
			let flag = Ipld::Bool(true);
			*IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &flag)
				.unwrap()
				.cid()
		};

		let msg = prepare_block_cid_fact_message(block, cid);
		let (block_dec, cid_dec) = decode_block_cid_fact_message(msg.unwrap()).unwrap();

		assert_eq!(block, block_dec);
		assert_eq!(cid, cid_dec);
	}

	fn matrix_strategy() -> impl Strategy<Value = Matrix> {
		let rows_len = (1..64usize).next().unwrap();
		collection::vec(
			collection::vec(any::<Option<Vec<u8>>>(), rows_len),
			collection::size_range(1..64),
		)
		.prop_map(move |columns| Matrix { columns })
	}

	proptest! {
		#[test]
		fn matrix_cells_length_is_correct(rows in 0u16..1024, cols in 0u16..1024) {
			prop_assert_eq!(matrix_cells(rows, cols).count(), rows as usize * cols as usize);
		}

		#[test]
		fn empty_cells_length_is_correct(matrix in matrix_strategy()) {
			let cols = matrix.columns.len() ;
			let rows = matrix.columns[0].len() ;
			let empty_cells_len = empty_cells(&matrix, cols as u16, rows as u16).len();
			let non_empty_cells_len =  non_empty_cells_len(&matrix);

			prop_assert_eq!(empty_cells_len + non_empty_cells_len, (rows * cols) as usize);
		}
	}

	#[test_case(1, 1 => vec![(0, 0)] ; "one cell")]
	#[test_case(4, 1 => vec![(0, 0), (1, 0), (2,0), (3,0)] ; "four rows, one column")]
	#[test_case(1, 4 => vec![(0, 0), (0, 1), (0,2), (0,3)] ; "four columns, one row")]
	#[test_case(2, 2 => vec![(0, 0), (1, 0), (0,1), (1,1)] ; "square matrix")]
	fn test_matrix_cells(rows: u16, cols: u16) -> Vec<(usize, usize)> {
		matrix_cells(rows, cols).collect::<Vec<(usize, usize)>>()
	}

	#[test]
	fn fact_message_decoding_failure() {
		// 256 bytes of random data
		let msg = random_data(256);
		assert_eq!(decode_block_cid_fact_message(msg), None);
	}

	#[test]
	fn ask_message_encoding_decoding_success_0() {
		let block: i128 = 1 << 31;
		// notice Cid is known for this ask message
		// denoting it's an answer to some question mesasge
		let cid: Cid = {
			let flag = Ipld::Bool(true);
			*IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &flag)
				.unwrap()
				.cid()
		};

		let msg = prepare_block_cid_ask_message(block, Some(cid));
		let (block_dec, cid_dec) = decode_block_cid_ask_message(msg.unwrap()).unwrap();

		assert_eq!(block, block_dec);
		assert_eq!(cid, cid_dec.unwrap());
	}

	#[test]
	fn ask_message_encoding_decoding_success_1() {
		let block: i128 = 1 << 31;
		// notice Cid is unknown is this case, denoting
		// it's question message, asked by some peer
		//
		// as good peer, responsibility is responding
		// back on same channel with block, cid pair
		let cid = None;

		let msg = prepare_block_cid_ask_message(block, cid);
		let (block_dec, cid_dec) = decode_block_cid_ask_message(msg.unwrap()).unwrap();

		assert_eq!(block, block_dec);
		assert_eq!(cid_dec, None);
	}

	#[test]
	fn ask_message_decoding_failure() {
		// 256 bytes of random data
		let msg = random_data(256);
		assert_eq!(decode_block_cid_ask_message(msg), None);
	}
}
