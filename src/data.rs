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

pub async fn construct_cell(block: u64, cell: Cell) -> BaseCell {
    let data = Ipld::Bytes(get_kate_query_proof_by_cell(block, cell).await.unwrap());
    IpldCodec::encode(IpldCodec::DagCbor, Code::Blake3_256, &data).unwrap()
}
