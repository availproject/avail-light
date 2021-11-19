extern crate ipfs_embed;
extern crate libipld;

use crate::rpc::{get_kate_query_proof_by_cell, Cell};
use ipfs_embed::{Block, DefaultParams};
use libipld::codec_impl::IpldCodec;
use libipld::multihash::Code;
use libipld::Ipld;

pub type IpldBlock = Block<DefaultParams>;
pub type BaseCell = IpldBlock;

#[derive(Clone)]
pub struct L0Col {
    pub base_cells: Vec<BaseCell>,
}

#[derive(Clone)]
pub struct L1Row {
    pub l0_cols: Vec<L0Col>,
}

#[derive(Clone)]
pub struct DataMatrix {
    pub block_num: i128,
    pub l1_row: L1Row,
}

pub async fn construct_cell(block: u64, row: u16, col: u16) -> BaseCell {
    let data = Ipld::Bytes(get_kate_query_proof_by_cell(block, row, col).await.unwrap());
    IpldCodec::encode(IpldCodec::DagCbor, Code::Blake3_256, &data).unwrap()
}

pub async fn construct_colwise(block: u64, row_count: u16, col: u16) -> L0Col {
    let mut base_cells: Vec<BaseCell> = Vec::new();

    for row in 0..row_count {
        base_cells.push(construct_cell(block, row, col).await);
    }

    L0Col {
        base_cells: base_cells,
    }
}

pub async fn construct_rowwise(block: u64, row_count: u16, col_count: u16) -> L1Row {
    let mut l0_cols: Vec<L0Col> = Vec::new();

    for col in 0..col_count {
        l0_cols.push(construct_colwise(block, row_count, col).await);
    }

    L1Row { l0_cols: l0_cols }
}

pub async fn construct_matrix(block: u64, row_count: u16, col_count: u16) -> DataMatrix {
    DataMatrix {
        l1_row: construct_rowwise(block, row_count, col_count).await,
        block_num: block as i128,
    }
}
