extern crate anyhow;
extern crate ipfs_embed;
extern crate libipld;

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use ipfs_embed::{Cid, DefaultParams, Ipfs, TempPin};
use libipld::{
	codec_impl::IpldCodec,
	multihash::{Code, MultihashDigest},
	Ipld,
};

use crate::{
	error::{self, IpldField},
	types::{BaseCell, DataMatrix, IpldBlock, L0Col, L1Row},
};

fn construct_cell(
	row: u16,
	col: u16,
	row_count: u16,
	cells: &[Option<Vec<u8>>],
) -> Result<BaseCell, error::App> {
	let cell = cells[(col * row_count + row) as usize]
		.as_ref()
		.ok_or(error::App::CellDataNotAvailable(row, col))?
		.clone();

	let data = Ipld::Bytes(cell);
	let encoded_cell = BaseCell::encode(IpldCodec::DagCbor, Code::Blake3_256, &data)
		.expect("`BaseCell` is encodable by design .qed");

	Ok(encoded_cell)
}

fn construct_colwise(
	row_count: u16,
	col: u16,
	cells: &[Option<Vec<u8>>],
) -> Result<L0Col, error::App> {
	let base_cells = (0..row_count)
		.map(|row| construct_cell(row, col, row_count, cells))
		.collect::<Result<Vec<_>, _>>()?;

	Ok(L0Col { base_cells })
}

pub fn construct_matrix(
	block: u64,
	row_count: u16,
	col_count: u16,
	cells: &[Option<Vec<u8>>],
) -> Result<DataMatrix, error::App> {
	let l0_cols = (0..col_count)
		.map(move |col| construct_colwise(row_count, col, cells))
		.collect::<Result<Vec<_>, _>>()?;

	Ok(DataMatrix {
		l1_row: L1Row { l0_cols },
		block_num: block as i128,
	})
}

pub type Cell = Vec<u8>;
pub type Column = Vec<Option<Cell>>;
pub type Matrix = Vec<Column>;

pub fn matrix_cells(rows: u16, cols: u16) -> impl Iterator<Item = (usize, usize)> {
	(0..cols as usize).flat_map(move |col| (0..rows as usize).map(move |row| (row, col)))
}

pub fn empty_cells(matrix: &Matrix, cols: u16, rows: u16) -> Vec<(usize, usize)> {
	// TODO: Optimal solution is to use proper Matrix abstraction to derive empty cells
	matrix_cells(rows, cols)
		.filter(|(row, col)| {
			matrix
				.get(*col)
				.and_then(|col| col.get(*row))
				.map(|cell| cell.is_none())
				.unwrap_or(true)
		})
		.collect::<Vec<(usize, usize)>>()
}

pub fn non_empty_cells_len(matrix: &Matrix) -> usize {
	matrix
		.iter()
		.fold(0usize, |sum, val| sum + val.iter().flatten().count())
}

fn get_cell(ipfs: &Ipfs<DefaultParams>, cid: &Cid) -> Result<Option<Cell>> {
	ipfs.get(cid)
		.and_then(|result| result.ipld())
		.map(|decoded| extract_cell(&decoded))
		.context("Cannot get cell")
}

fn get_column(ipfs: &Ipfs<DefaultParams>, cid: &Cid) -> Result<Column> {
	let links = ipfs
		.get(cid)
		.and_then(|result| result.ipld())
		.and_then(|decoded| extract_links(&decoded).context("Cannot extract cell links"))
		.context("Cannot get cell links")?;

	links
		.iter()
		.flat_map(|link| link.context("Cell link is missing"))
		.map(|cell_cid| get_cell(ipfs, &cell_cid))
		.collect::<Result<Column>>()
		.context("Cannot get column cells")
}

pub fn get_matrix(ipfs: &Ipfs<DefaultParams>, root_cid: Option<Cid>) -> Result<Matrix> {
	match root_cid {
		None => Ok(vec![]),
		Some(cid) => {
			let column_cids = ipfs
				.get(&cid)
				.and_then(|result| result.ipld())
				.and_then(|root| destructure_matrix(&root).context("Cannot destructure root block"))
				.and_then(|(_, column_cids, _)| column_cids.context("No column block cids"))
				.context("Cannot get column cids")?;

			column_cids
				.iter()
				.flat_map(|column_cid| column_cid.context("No column block cid"))
				.map(|column_cid| get_column(ipfs, &column_cid))
				.collect::<Result<Matrix>>()
				.context("Cannot get matrix")
		},
	}
}

fn push_cell(cell: BaseCell, ipfs: &Ipfs<DefaultParams>, pin: &TempPin) -> Result<Cid, error::App> {
	let _ = ipfs
		.temp_pin(pin, cell.cid())
		.map_err(|_| error::IpfsClient::PinFailed(IpldField::Cell))?;

	ipfs.insert(&cell)
		.map_err(|_| error::IpfsClient::PinFailed(IpldField::Cell))?;

	Ok(*cell.cid())
}

async fn push_col(
	col: L0Col,
	ipfs: &Ipfs<DefaultParams>,
	pin: &TempPin,
) -> Result<Cid, error::App> {
	let cids = col
		.base_cells
		.into_iter()
		.map(|cell| push_cell(cell, ipfs, pin))
		.try_collect::<Vec<_>>()?;
	let cells = cids.into_iter().map(Ipld::Link).collect::<Vec<_>>();

	let col = Ipld::List(cells);
	let coded_col = IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &col)
		.expect("`IpldBlock` encode is valid by desing .qed");
	let _ = ipfs
		.temp_pin(pin, coded_col.cid())
		.map_err(|_| error::IpfsClient::PinFailed(IpldField::Columns))?;
	let _ = ipfs
		.insert(&coded_col)
		.map_err(|_| error::IpfsClient::InsertFailed(IpldField::Columns))?;

	Ok(*coded_col.cid())
}

async fn push_row(
	row: L1Row,
	block_num: i128,
	latest_cid: Option<Cid>,
	ipfs: &Ipfs<DefaultParams>,
	pin: &TempPin,
) -> anyhow::Result<Cid, error::App> {
	let mut col_cids: Vec<Ipld> = Vec::with_capacity(row.l0_cols.len());
	for col in row.l0_cols {
		let cid = push_col(col, ipfs, pin).await?;
		col_cids.push(Ipld::Link(cid));
	}

	let map = BTreeMap::from([
		("columns".to_owned(), Ipld::List(col_cids)),
		("block".to_owned(), Ipld::Integer(block_num)),
		(
			"prev".to_owned(),
			latest_cid.map(Ipld::Link).unwrap_or(Ipld::Null),
		),
	]);

	let map = Ipld::StringMap(map);

	let coded_mat = IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &map)
		.expect("`IpldBlock` is encodable by design .qed");

	let _ = ipfs
		.temp_pin(pin, coded_mat.cid())
		.map_err(|_| error::IpfsClient::PinFailed(IpldField::Rows))?;

	let _ = ipfs
		.insert(&coded_mat)
		.map_err(|_| error::IpfsClient::InsertFailed(IpldField::Rows))?;

	Ok(*coded_mat.cid())
}

pub async fn push_matrix(
	data_matrix: DataMatrix,
	latest_cid: Option<Cid>,
	ipfs: &Ipfs<DefaultParams>,
	pin: &TempPin,
) -> anyhow::Result<Cid, error::App> {
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
			let block = map.get("block").and_then(extract_block);
			let cols = map.get("columns").and_then(extract_links);
			let prev = map.get("prev").and_then(extract_cid);

			if block.is_some() && cols.is_some() && prev.is_some() {
				Some((block, cols, prev))
			} else {
				None
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
pub fn prepare_block_cid_fact_message(block: i128, cid: Cid) -> Vec<u8> {
	let inner_map = BTreeMap::from([
		("block".to_owned(), Ipld::Integer(block)),
		("cid".to_owned(), Ipld::Link(cid)),
	]);
	let map = Ipld::StringMap(inner_map);

	IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &map)
		.expect("`IpldBlock` encoding is valid by design .qed")
		.data()
		.to_vec()
}

// Takes a IPLD coded fact channel message as byte array, which is
// received from other peer on fact topic, and attempts
// to decode it into two constituent components i.e. block number
// and respective Cid of block data matrix
pub fn decode_block_cid_fact_message(msg: Vec<u8>) -> Option<(i128, Cid)> {
	let m_hash = Code::Blake3_256.digest(&msg[..]);
	let cid = Cid::new_v1(IpldCodec::DagCbor.into(), m_hash);

	let coded_msg = IpldBlock::new(cid, msg).ok()?;
	let decoded_msg = coded_msg.ipld().ok()?;

	match decoded_msg {
		Ipld::StringMap(map) => {
			let map: BTreeMap<String, Ipld> = map;
			let block = map.get("block").and_then(extract_block)?;
			let cid = map.get("cid").and_then(extract_cid)?;
			Some((block, cid))
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
pub fn prepare_block_cid_ask_message(block: i128, maybe_cid: Option<Cid>) -> Vec<u8> {
	let cid = maybe_cid.map(Ipld::Link).unwrap_or(Ipld::Null);
	let inner_map = BTreeMap::from([
		("block".to_owned(), Ipld::Integer(block)),
		("cid".to_owned(), cid),
	]);
	let map = Ipld::StringMap(inner_map);

	IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &map)
		.expect("`IpldBlock` encode is valid by design .qed")
		.data()
		.to_vec()
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
	let coded_msg = IpldBlock::new(cid, msg).ok()?;
	let msg = coded_msg.ipld().ok()?;

	match msg {
		Ipld::StringMap(map) => {
			let maybe_block = map.get("block").and_then(extract_block);
			let maybe_cid = map.get("cid").and_then(extract_cid);

			maybe_block.map(|block| (block, maybe_cid))
		},
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	extern crate rand;

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
		let (block_dec, cid_dec) = decode_block_cid_fact_message(msg).unwrap();

		assert_eq!(block, block_dec);
		assert_eq!(cid, cid_dec);
	}

	fn matrix_strategy() -> impl Strategy<Value = Vec<Vec<Option<Vec<u8>>>>> {
		let rows_len = (1..64usize).next().unwrap();
		collection::vec(
			collection::vec(any::<Option<Vec<u8>>>(), rows_len),
			collection::size_range(1..64),
		)
	}

	proptest! {
		#[test]
		fn matrix_cells_length_is_correct(rows in 0u16..1024, cols in 0u16..1024) {
			prop_assert_eq!(matrix_cells(rows, cols).count(), rows as usize * cols as usize);
		}

		#[test]
		fn empty_cells_length_is_correct(matrix in matrix_strategy()) {
			let cols = matrix.len() ;
			let rows = matrix[0].len() ;
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
		let (block_dec, cid_dec) = decode_block_cid_ask_message(msg).unwrap();

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
		let (block_dec, cid_dec) = decode_block_cid_ask_message(msg).unwrap();

		assert_eq!(block, block_dec);
		assert_eq!(cid_dec, None);
	}

	#[test]
	fn ask_message_decoding_failure() {
		// 256 bytes of random data
		let msg = random_data(256);
		assert_eq!(decode_block_cid_ask_message(msg), None);
	}

	#[tokio::test]
	async fn test_data_matrix_coding_decoding_flow() {
		let ipfs = make_client(1, 10000, "test").await.unwrap();
		let block: u64 = 1 << 63;
		let row_c = 4;
		let col_c = 4;
		let cells = &[
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
			Some(random_data(80)),
		];
		let data_matrix = construct_matrix(block, row_c, col_c, cells).unwrap();
		let prev_cid: Cid = {
			let flag = Ipld::Bool(true);
			*IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &flag)
				.unwrap()
				.cid()
		};
		let pin = ipfs.create_temp_pin().unwrap();
		let root_cid = push_matrix(data_matrix, Some(prev_cid), &ipfs, &pin)
			.await
			.unwrap();

		let result = get_matrix(&ipfs, Some(root_cid)).unwrap();

		let mut cells_iter = cells.iter();

		for col in result {
			for cell in col {
				assert_eq!(cells_iter.next().unwrap().as_ref().unwrap(), &cell.unwrap());
			}
		}
	}
}
