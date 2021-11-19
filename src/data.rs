extern crate anyhow;
extern crate ipfs_embed;
extern crate libipld;

use crate::rpc::get_kate_query_proof_by_cell;
use crate::types::{BaseCell, DataMatrix, IpldBlock, L0Col, L1Row};
use ipfs_embed::{Cid, DefaultParams, Ipfs, TempPin};
use libipld::codec_impl::IpldCodec;
use libipld::multihash::Code;
use libipld::Ipld;
use std::collections::BTreeMap;

async fn construct_cell(block: u64, row: u16, col: u16) -> BaseCell {
    let data = Ipld::Bytes(get_kate_query_proof_by_cell(block, row, col).await);
    IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &data).unwrap()
}

async fn construct_colwise(block: u64, row_count: u16, col: u16) -> L0Col {
    let mut base_cells: Vec<BaseCell> = Vec::with_capacity(row_count as usize);

    for row in 0..row_count {
        base_cells.push(construct_cell(block, row, col).await);
    }

    L0Col {
        base_cells: base_cells,
    }
}

async fn construct_rowwise(block: u64, row_count: u16, col_count: u16) -> L1Row {
    let mut l0_cols: Vec<L0Col> = Vec::with_capacity(col_count as usize);

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

async fn push_cell(
    cell: BaseCell,
    ipfs: &Ipfs<DefaultParams>,
    pin: &TempPin,
) -> anyhow::Result<Cid> {
    ipfs.temp_pin(pin, cell.cid())?;
    ipfs.insert(&cell)?;

    Ok(*cell.cid())
}

async fn push_col(col: L0Col, ipfs: &Ipfs<DefaultParams>, pin: &TempPin) -> anyhow::Result<Cid> {
    let mut cell_cids: Vec<Ipld> = Vec::with_capacity(col.base_cells.len());

    for cell in col.base_cells {
        if let Ok(cid) = push_cell(cell, ipfs, pin).await {
            cell_cids.push(Ipld::Link(cid));
        };
    }

    let col = Ipld::List(cell_cids);
    let coded_col = IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &col).unwrap();

    ipfs.temp_pin(pin, coded_col.cid())?;
    ipfs.insert(&coded_col)?;

    Ok(*coded_col.cid())
}

async fn push_row(
    row: L1Row,
    block_num: i128,
    latest_cid: Option<Cid>,
    ipfs: &Ipfs<DefaultParams>,
    pin: &TempPin,
) -> anyhow::Result<Cid> {
    let mut col_cids: Vec<Ipld> = Vec::with_capacity(row.l0_cols.len());

    for col in row.l0_cols {
        if let Ok(cid) = push_col(col, ipfs, pin).await {
            col_cids.push(Ipld::Link(cid));
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
    let coded_matrix = IpldBlock::encode(IpldCodec::DagCbor, Code::Blake3_256, &map).unwrap();

    ipfs.temp_pin(pin, coded_matrix.cid())?;
    ipfs.insert(&coded_matrix)?;

    Ok(*coded_matrix.cid())
}

pub async fn push_matrix(
    data_matrix: DataMatrix,
    latest_cid: Option<Cid>,
    ipfs: &Ipfs<DefaultParams>,
    pin: &TempPin,
) -> anyhow::Result<Cid> {
    Ok(push_row(
        data_matrix.l1_row,
        data_matrix.block_num,
        latest_cid,
        ipfs,
        pin,
    )
    .await?)
}
