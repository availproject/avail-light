use hyper;
use hyper_tls::HttpsConnector;
use rand::{thread_rng, Rng};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::{Arc, Mutex};

pub type Store = Arc<Mutex<HashMap<u64, u32>>>;

#[derive(Deserialize, Debug)]
pub struct BlockHashResponse {
    jsonrpc: String,
    id: u32,
    result: String,
}

#[derive(Deserialize, Debug)]
pub struct BlockResponse {
    jsonrpc: String,
    id: u32,
    result: RPCResult,
}

#[derive(Deserialize, Debug)]
pub struct RPCResult {
    block: Block,
    #[serde(skip_deserializing)]
    justification: String,
}

#[derive(Deserialize, Debug)]
pub struct Block {
    extrinsics: Vec<String>,
    pub header: Header,
}

#[derive(Deserialize, Debug)]
pub struct Header {
    number: String,
    #[serde(rename = "extrinsicsRoot")]
    pub extrinsics_root: ExtrinsicsRoot,
    #[serde(rename = "parentHash")]
    parent_hash: String,
    #[serde(rename = "stateRoot")]
    state_root: String,
    digest: Digest,
}

#[derive(Deserialize, Debug)]
pub struct ExtrinsicsRoot {
    pub cols: u16,
    pub rows: u16,
    hash: String,
    pub commitment: Vec<u8>,
}

#[derive(Deserialize, Debug)]
pub struct Digest {
    logs: Vec<String>,
}

#[derive(Deserialize, Debug)]
pub struct BlockProofResponse {
    jsonrpc: String,
    id: u32,
    result: Vec<u8>,
}

#[derive(Default, Debug)]
pub struct Cell {
    pub block: u64,
    pub row: u16,
    pub col: u16,
    pub proof: Vec<u8>,
}

#[derive(Hash, Eq, PartialEq)]
pub struct MatrixCell {
    row: u16,
    col: u16,
}

fn get_full_node_url() -> String {
    if let Ok(v) = env::var("FullNodeURL") {
        v
    } else {
        "http://localhost:9999".to_owned()
    }
}

fn is_secure(url: &str) -> bool {
    let re = Regex::new(r"^https://.*").unwrap();
    re.is_match(url)
}

pub fn get_host() -> String {
    if let Ok(v) = env::var("Host") {
        v
    } else {
        "127.0.0.1".to_owned()
    }
}

pub fn get_port() -> u16 {
    if let Ok(v) = env::var("Port") {
        v.parse::<u16>().unwrap()
    } else {
        7000
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

pub async fn get_kate_proof(
    block: u64,
    max_rows: u16,
    max_cols: u16,
) -> Result<Vec<Cell>, String> {
    let mut cells = generate_random_cells(max_rows, max_cols, block);
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
