#![allow(dead_code)]
use crate::types::*;
use dotenv::dotenv;
use hyper;
use hyper_tls::HttpsConnector;
use rand::{thread_rng, Rng};
use regex::Regex;
use std::collections::HashSet;
use std::env;

pub fn get_ws_node_url() -> String {
    dotenv().ok();
    if let Ok(v) = env::var("FullNodeWSURL") {
        v
    } else {
        "ws://localhost:9944".to_owned()
    }
}

pub fn get_full_node_url() -> String {
    dotenv().ok();
    if let Ok(v) = env::var("FullNodeURL") {
        v
    } else {
        "http://localhost:9933".to_owned()
    }
}

fn is_secure(url: &str) -> bool {
    let re = Regex::new(r"^https://.*").unwrap();
    re.is_match(url)
}

pub fn get_port() -> u16 {
    dotenv().ok();
    if let Ok(v) = env::var("Port") {
        v.parse::<u16>().unwrap()
    } else {
        7000
    }
}

pub fn get_app_id() -> u16 {
    dotenv().ok();
    if let Ok(v) = env::var("APPID") {
        v.parse::<u16>().unwrap()
    } else {
        0
    }
}

pub async fn get_blockhash(block: u64) -> Result<String, String> {
    let payload = format!(
        r#"{{"id": 1, "jsonrpc": "2.0", "method": "chain_getBlockHash", "params": [{}]}}"#,
        block
    );
    let req = hyper::Request::builder()
        .method(hyper::Method::POST)
        .uri(get_full_node_url())
        .header("Content-Type", "application/json")
        .body(hyper::Body::from(payload))
        .unwrap();
    let resp = if is_secure(&get_full_node_url()) {
        let https = HttpsConnector::new();
        let client = hyper::Client::builder().build::<_, hyper::Body>(https);
        client.request(req).await.unwrap()
    } else {
        let client = hyper::Client::new();
        client.request(req).await.unwrap()
    };
    let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
    let r: BlockHashResponse = serde_json::from_slice(&body).unwrap();
    Ok(r.result)
}

pub async fn get_block_by_hash(hash: String) -> Result<Block, String> {
    let payload = format!(
        r#"{{"id": 1, "jsonrpc": "2.0", "method": "chain_getBlock", "params": ["{}"]}}"#,
        hash
    );
    let req = hyper::Request::builder()
        .method(hyper::Method::POST)
        .uri(get_full_node_url())
        .header("Content-Type", "application/json")
        .body(hyper::Body::from(payload))
        .unwrap();
    let resp = if is_secure(&get_full_node_url()) {
        let https = HttpsConnector::new();
        let client = hyper::Client::builder().build::<_, hyper::Body>(https);
        client.request(req).await.unwrap()
    } else {
        let client = hyper::Client::new();
        client.request(req).await.unwrap()
    };
    let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
    let b: BlockResponse = serde_json::from_slice(&body).unwrap();
    Ok(b.result.block)
}

pub async fn get_block_by_number(block: u64) -> Result<Block, String> {
    let hash = get_blockhash(block).await.unwrap();
    Ok(get_block_by_hash(hash).await.unwrap())
}

pub fn generate_random_cells(max_rows: u16, max_cols: u16, block: u64) -> Vec<Cell> {
    let count: u16 = if max_rows * max_cols < 8 {
        max_rows * max_cols
    } else {
        8
    };
    let mut rng = thread_rng();
    let mut indices = HashSet::new();
    while (indices.len() as u16) < count {
        let row = rng.gen::<u16>() % max_rows;
        let col = rng.gen::<u16>() % max_cols;
        indices.insert(MatrixCell { row: row, col: col });
    }

    let mut buf = Vec::new();
    for index in indices {
        buf.push(Cell {
            block: block,
            row: index.row,
            col: index.col,
            ..Default::default()
        });
    }
    buf
}

pub fn generate_kate_query_payload(block: u64, cells: &Vec<Cell>) -> String {
    let mut query = Vec::new();
    for cell in cells {
        query.push(format!(r#"{{"row": {}, "col": {}}}"#, cell.row, cell.col));
    }
    format!(
        r#"{{"id": 1, "jsonrpc": "2.0", "method": "kate_queryProof", "params": [{}, [{}]]}}"#,
        block,
        query.join(", ")
    )
}

pub fn fill_cells_with_proofs(cells: &mut Vec<Cell>, proof: &BlockProofResponse) {
    assert_eq!(80 * cells.len(), proof.result.len());
    for i in 0..cells.len() {
        let mut v = Vec::new();
        v.extend_from_slice(&proof.result[i * 80..i * 80 + 80]);
        cells[i].proof = v;
    }
}

// Get proof of certain cell for given block, from full node
pub async fn get_kate_query_proof_by_cell(block: u64, row: u16, col: u16) -> Vec<u8> {
    let payload: String = format!(
        r#"{{"id": 1, "jsonrpc": "2.0", "method": "kate_queryProof", "params": [{}, [{}]]}}"#,
        block,
        format!(r#"{{"row": {}, "col": {}}}"#, row, col)
    );

    let req = hyper::Request::builder()
        .method(hyper::Method::POST)
        .uri(get_full_node_url())
        .header("Content-Type", "application/json")
        .body(hyper::Body::from(payload))
        .unwrap();
    let resp = if is_secure(&get_full_node_url()) {
        let https = HttpsConnector::new();
        let client = hyper::Client::builder().build::<_, hyper::Body>(https);
        client.request(req).await.unwrap()
    } else {
        let client = hyper::Client::new();
        client.request(req).await.unwrap()
    };
    let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
    let proof: BlockProofResponse = serde_json::from_slice(&body).unwrap();

    proof.result
}

pub async fn get_kate_proof(
    block: u64,
    max_rows: u16,
    max_cols: u16,
    app_id: bool,
) -> Result<Vec<Cell>, String> {
    let num = get_block_by_number(block).await.unwrap();
    let app_index = num.header.app_data_lookup.index;
    let app_size = num.header.app_data_lookup.size;

    //checking for if the user is subscribed for a particular APPID
    let mut cells = if app_id == false {
        let cpy = generate_random_cells(max_rows, max_cols, block);
        cpy
    } else {
        let app_tup = app_index[0];
        let app_ind = app_tup.1;
        let cpy = generate_app_specific_cells(app_size, app_ind, max_rows, max_cols, block);
        cpy
    };
    let payload = generate_kate_query_payload(block, &cells);
    let req = hyper::Request::builder()
        .method(hyper::Method::POST)
        .uri(get_full_node_url())
        .header("Content-Type", "application/json")
        .body(hyper::Body::from(payload))
        .unwrap();
    let resp = if is_secure(&get_full_node_url()) {
        let https = HttpsConnector::new();
        let client = hyper::Client::builder().build::<_, hyper::Body>(https);
        client.request(req).await.unwrap()
    } else {
        let client = hyper::Client::new();
        client.request(req).await.unwrap()
    };
    let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
    let proof: BlockProofResponse = serde_json::from_slice(&body).unwrap();
    fill_cells_with_proofs(&mut cells, &proof);
    Ok(cells)
}

pub fn generate_app_specific_cells(
    size: u32,
    index: u32,
    max_rows: u16,
    max_col: u16,
    block: u64,
) -> Vec<Cell> {
    let mut buf = Vec::new();
    let rows: u16 = 0;
    for i in 0..=size {
        let rows = if rows < max_rows {
            (index - 1) as u16 / max_col
        } else {
            ((index - 1) as u16 / max_col) + i as u16
        };
        let cols = ((index - 1) as u16 % max_col) + i as u16;
        buf.push(Cell {
            block: block,
            row: rows as u16,
            col: cols as u16,
            ..Default::default()
        });
    }
    buf
}
