extern crate anyhow;
extern crate dusk_plonk;
extern crate ipfs_embed;
extern crate libipld;

use crate::recovery::reconstruct_poly;
use crate::rpc::get_kate_query_proof_by_cell;
use crate::types::{BaseCell, Cell, DataMatrix, IpldBlock, L0Col, L1Row};
use dusk_plonk::bls12_381::BlsScalar;
use dusk_plonk::fft::EvaluationDomain;
use ipfs_embed::{Cid, DefaultParams, Ipfs, TempPin};
use libipld::codec_impl::IpldCodec;
use libipld::multihash::{Code, MultihashDigest};
use libipld::Ipld;
use std::collections::BTreeMap;
use std::convert::TryInto;

async fn construct_cell(url: &str, block: u64, row: u16, col: u16) -> Result<BaseCell, String> {
    match get_kate_query_proof_by_cell(url, block, row, col).await {
        Ok(cell) => {
            let data = Ipld::Bytes(cell);
            match IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &data) {
                Ok(coded_cell) => Ok(coded_cell),
                Err(_) => Err("failed to IPLD encode cell of data matrix".to_owned()),
            }
        }
        Err(msg) => Err(msg),
    }
}

async fn construct_colwise(
    url: &str,
    block: u64,
    row_count: u16,
    col: u16,
) -> Result<L0Col, String> {
    let mut base_cells: Vec<BaseCell> = Vec::with_capacity(row_count as usize);

    for row in 0..row_count {
        let cell = match construct_cell(url, block, row, col).await {
            Ok(cell) => {
                base_cells.push(cell);
            }
            Err(msg) => return Err(msg),
        };
    }

    Ok(L0Col {
        base_cells: base_cells,
    })
}

async fn construct_rowwise(
    url: &str,
    block: u64,
    row_count: u16,
    col_count: u16,
) -> Result<L1Row, String> {
    let mut l0_cols: Vec<L0Col> = Vec::with_capacity(col_count as usize);

    for col in 0..col_count {
        match construct_colwise(url, block, row_count, col).await {
            Ok(col) => {
                l0_cols.push(col);
            }
            Err(msg) => return Err(msg),
        };
    }

    Ok(L1Row { l0_cols: l0_cols })
}

pub async fn construct_matrix(
    url: &str,
    block: u64,
    row_count: u16,
    col_count: u16,
) -> Result<DataMatrix, String> {
    match construct_rowwise(url, block, row_count, col_count).await {
        Ok(row) => Ok(DataMatrix {
            l1_row: row,
            block_num: block as i128,
        }),
        Err(msg) => return Err(msg),
    }
}

async fn push_cell(
    cell: BaseCell,
    ipfs: &Ipfs<DefaultParams>,
    pin: &TempPin,
) -> anyhow::Result<Cid, String> {
    match ipfs.temp_pin(pin, cell.cid()) {
        Ok(_) => match ipfs.insert(&cell) {
            Ok(_) => Ok(*cell.cid()),
            Err(_) => Err("failed to IPFS insert cell of data matrix".to_owned()),
        },
        Err(_) => Err("failed to IPFS pin cell of data matrix".to_owned()),
    }
}

async fn push_col(
    col: L0Col,
    ipfs: &Ipfs<DefaultParams>,
    pin: &TempPin,
) -> anyhow::Result<Cid, String> {
    let mut cell_cids: Vec<Ipld> = Vec::with_capacity(col.base_cells.len());

    for cell in col.base_cells {
        match push_cell(cell, ipfs, pin).await {
            Ok(cid) => {
                cell_cids.push(Ipld::Link(cid));
            }
            Err(msg) => return Err(msg),
        };
    }

    let col = Ipld::List(cell_cids);
    match IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &col) {
        Ok(coded_col) => match ipfs.temp_pin(pin, coded_col.cid()) {
            Ok(_) => match ipfs.insert(&coded_col) {
                Ok(_) => Ok(*coded_col.cid()),
                Err(_) => Err("failed to IPFS insert column of data matrix".to_owned()),
            },
            Err(_) => Err("failed to IPFS pin column of data matrix".to_owned()),
        },
        Err(_) => Err("failed to IPLD encode column of data matrix".to_owned()),
    }
}

async fn push_row(
    row: L1Row,
    block_num: i128,
    latest_cid: Option<Cid>,
    ipfs: &Ipfs<DefaultParams>,
    pin: &TempPin,
) -> anyhow::Result<Cid, String> {
    let mut col_cids: Vec<Ipld> = Vec::with_capacity(row.l0_cols.len());

    for col in row.l0_cols {
        match push_col(col, ipfs, pin).await {
            Ok(cid) => {
                col_cids.push(Ipld::Link(cid));
            }
            Err(msg) => {
                return Err(msg);
            }
        };
    }

    let mut map = BTreeMap::new();

    map.insert("columns".to_owned(), Ipld::List(col_cids));
    map.insert("block".to_owned(), Ipld::Integer(block_num));
    map.insert(
        "prev".to_owned(),
        match latest_cid {
            Some(cid) => Ipld::Link(cid),
            None => Ipld::Null,
        },
    );

    let map = Ipld::StringMap(map);
    match IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &map) {
        Ok(coded_mat) => match ipfs.temp_pin(pin, coded_mat.cid()) {
            Ok(_) => match ipfs.insert(&coded_mat) {
                Ok(_) => Ok(*coded_mat.cid()),
                Err(_) => Err("failed to IPFS insert row of data matrix".to_owned()),
            },
            Err(_) => Err("failed to IPFS pin row of data matrix".to_owned()),
        },
        Err(_) => Err("failed to IPLD encode row of data matrix".to_owned()),
    }
}

pub async fn push_matrix(
    data_matrix: DataMatrix,
    latest_cid: Option<Cid>,
    ipfs: &Ipfs<DefaultParams>,
    pin: &TempPin,
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

// Extracts respective CID block number from IPLD
// encapsulated data object
fn extract_cid(data: &Ipld) -> Option<Cid> {
    match data {
        Ipld::Link(cid) => Some(*cid),
        Ipld::Null => None,
        _ => None,
    }
}

// Extracts block number from IPLD encapsulated data object
fn extract_block(data: &Ipld) -> Option<i128> {
    match data {
        Ipld::Integer(block) => Some(*block),
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
    let mut map = BTreeMap::new();

    map.insert("block".to_owned(), Ipld::Integer(block));
    map.insert("cid".to_owned(), Ipld::Link(cid));

    let map = Ipld::StringMap(map);
    let coded = IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &map).unwrap();

    coded.data().to_vec()
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
        }
        _ => None,
    }
}

// Some peer who doesn't know about block data matrix CID of some specified
// block number, may need to know same, sends a message over pre-agreed upon
// channel ( topic ). I call this channel ask channel, where peers get to ask
// questions & expect someone will answer it
//
// Same channel will be used when attempting to answer back to question
pub fn prepare_block_cid_ask_message(block: i128, cid: Option<Cid>) -> Vec<u8> {
    let mut map = BTreeMap::new();

    map.insert("block".to_owned(), Ipld::Integer(block));
    map.insert(
        "cid".to_owned(),
        match cid {
            Some(cid) => Ipld::Link(cid),
            None => Ipld::Null,
        },
    );

    let map = Ipld::StringMap(map);
    let coded = IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &map).unwrap();

    coded.data().to_vec()
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
        }
        _ => None,
    }
}

// use this function for reconstructing back all cells of certain column
// when at least 50% of them are available
//
// if everything goes fine, returned vector in case of success should have
// `row_count`-many cells of some specific column, in coded form
//
// performing one round of ifft should reveal original data which were
// coded together
pub fn reconstruct_column(row_count: usize, cells: &[Cell]) -> Result<Vec<BlsScalar>, String> {
    // just ensures all rows are from same column !
    // it's required as that's how it's erasure coded during
    // construction in validator node
    fn check_cells(cells: &[Cell]) {
        assert!(cells.len() > 0);
        let col = cells[0].col;
        for cell in cells {
            assert_eq!(col, cell.col);
        }
    }

    // given row index in column of interest, finds it if present
    // and returns back wrapped in `Some`, otherwise returns `None`
    fn find_row_by_index(idx: usize, cells: &[Cell]) -> Option<BlsScalar> {
        for cell in cells {
            if cell.row == idx as u16 {
                return Some(
                    BlsScalar::from_bytes(
                        &cell.proof[..]
                            .try_into()
                            .expect("didn't find u8 array of length 32"),
                    )
                    .unwrap(),
                );
            }
        }
        None
    }

    // row count of data matrix must be power of two !
    assert!(row_count & (row_count - 1) == 0);
    assert!(cells.len() >= row_count / 2 && cells.len() <= row_count);
    check_cells(cells);

    let eval_domain = EvaluationDomain::new(row_count).unwrap();
    let mut subset: Vec<Option<BlsScalar>> = Vec::with_capacity(row_count);

    // fill up vector in ordered fashion
    // @note the way it's done should be improved
    for i in 0..row_count {
        subset.push(find_row_by_index(i, cells));
    }

    reconstruct_poly(eval_domain, subset)
}

#[cfg(test)]
mod tests {
    extern crate rand;

    use super::*;
    use rand::prelude::random;

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

    // Following test cases attempt to figure out any loop holes
    // I might be leaving, when reconstructing whole column of data
    // matrix when >= 50% of those cells along a certain column
    // are available

    #[test]
    fn reconstruct_column_success_0() {
        // This is fairly standard test
        //
        // In general it should be the way how this
        // function should be used

        let domain_size = 1usize << 2;
        let row_count = 2 * domain_size;
        let eval_domain = EvaluationDomain::new(row_count).unwrap();

        let mut src: Vec<BlsScalar> = Vec::with_capacity(row_count);
        for i in 0..domain_size {
            src.push(BlsScalar::from(1 << (i + 1)));
        }
        for _ in domain_size..row_count {
            src.push(BlsScalar::zero());
        }

        // erasure coded all data
        let coded = eval_domain.fft(&src);
        assert!(coded.len() == row_count);

        let cells = vec![
            Cell {
                row: 0,
                proof: coded[0].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 4,
                proof: coded[4].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 6,
                proof: coded[6].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 2,
                proof: coded[2].to_bytes().to_vec(),
                ..Default::default()
            },
        ];

        let reconstructed = reconstruct_column(row_count, &cells[..]).unwrap();
        for i in 0..row_count {
            assert_eq!(coded[i], reconstructed[i]);
        }
    }

    #[test]
    fn reconstruct_column_success_1() {
        // Just an extension of above test, where I also
        // attempt to decode back to original data and
        // run some assertions [ must work ! ]

        let domain_size = 1usize << 2;
        let row_count = 2 * domain_size;
        let eval_domain = EvaluationDomain::new(row_count).unwrap();

        let mut src: Vec<BlsScalar> = Vec::with_capacity(row_count);
        for i in 0..domain_size {
            src.push(BlsScalar::from(1 << (i + 1)));
        }
        for _ in domain_size..row_count {
            src.push(BlsScalar::zero());
        }

        // erasure coded all data
        let coded = eval_domain.fft(&src);
        assert!(coded.len() == row_count);

        let cells = vec![
            Cell {
                row: 0,
                proof: coded[0].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 4,
                proof: coded[4].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 6,
                proof: coded[6].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 2,
                proof: coded[2].to_bytes().to_vec(),
                ..Default::default()
            },
        ];

        let reconstructed = reconstruct_column(row_count, &cells[..]).unwrap();
        for i in 0..row_count {
            assert_eq!(coded[i], reconstructed[i]);
        }

        let decoded = eval_domain.ifft(&reconstructed);

        for i in 0..row_count {
            assert_eq!(src[i], decoded[i]);
        }
    }

    #[test]
    #[should_panic]
    fn reconstruct_column_failure_0() {
        // Notice how I attempt to construct `cells`
        // vector, I'm intentionally keeping duplicate data
        // so it must fail to reconstruct back [ will panic !]

        let domain_size = 1usize << 2;
        let row_count = 2 * domain_size;
        let eval_domain = EvaluationDomain::new(row_count).unwrap();

        let mut src: Vec<BlsScalar> = Vec::with_capacity(row_count);
        for i in 0..domain_size {
            src.push(BlsScalar::from(1 << (i + 1)));
        }
        for _ in domain_size..row_count {
            src.push(BlsScalar::zero());
        }

        // erasure coded all data
        let coded = eval_domain.fft(&src);
        assert!(coded.len() == row_count);

        let cells = vec![
            Cell {
                row: 0,
                proof: coded[0].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 0,
                proof: coded[0].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 6,
                proof: coded[6].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 2,
                proof: coded[2].to_bytes().to_vec(),
                ..Default::default()
            },
        ];

        let reconstructed = reconstruct_column(row_count, &cells[..]).unwrap();
        for i in 0..row_count {
            assert_eq!(coded[i], reconstructed[i]);
        }
    }

    #[test]
    #[should_panic]
    fn reconstruct_column_failure_1() {
        // Again notice how I'm constructing `cells`
        // vector, it must have at least 50% data available
        // to be able to reconstruct whole data back properly

        let domain_size = 1usize << 2;
        let row_count = 2 * domain_size;
        let eval_domain = EvaluationDomain::new(row_count).unwrap();

        let mut src: Vec<BlsScalar> = Vec::with_capacity(row_count);
        for i in 0..domain_size {
            src.push(BlsScalar::from(1 << (i + 1)));
        }
        for _ in domain_size..row_count {
            src.push(BlsScalar::zero());
        }

        // erasure coded all data
        let coded = eval_domain.fft(&src);
        assert!(coded.len() == row_count);

        let cells = vec![
            Cell {
                row: 4,
                proof: coded[4].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 6,
                proof: coded[6].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 2,
                proof: coded[2].to_bytes().to_vec(),
                ..Default::default()
            },
        ];

        let reconstructed = reconstruct_column(row_count, &cells[..]).unwrap();
        for i in 0..row_count {
            assert_eq!(coded[i], reconstructed[i]);
        }
    }

    #[test]
    #[should_panic]
    fn reconstruct_column_failure_2() {
        // Again check how I construct `cells` vector
        // where I put wrong row's data in place of wrong
        // row index [ will panic !]

        let domain_size = 1usize << 2;
        let row_count = 2 * domain_size;
        let eval_domain = EvaluationDomain::new(row_count).unwrap();

        let mut src: Vec<BlsScalar> = Vec::with_capacity(row_count);
        for i in 0..domain_size {
            src.push(BlsScalar::from(1 << (i + 1)));
        }
        for _ in domain_size..row_count {
            src.push(BlsScalar::zero());
        }

        // erasure coded all data
        let coded = eval_domain.fft(&src);
        assert!(coded.len() == row_count);

        let cells = vec![
            Cell {
                row: 0,
                proof: coded[0].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 5,
                proof: coded[4].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 6,
                proof: coded[6].to_bytes().to_vec(),
                ..Default::default()
            },
            Cell {
                row: 2,
                proof: coded[2].to_bytes().to_vec(),
                ..Default::default()
            },
        ];

        let reconstructed = reconstruct_column(row_count, &cells[..]).unwrap();
        for i in 0..row_count {
            assert_eq!(coded[i], reconstructed[i]);
        }
    }
}
