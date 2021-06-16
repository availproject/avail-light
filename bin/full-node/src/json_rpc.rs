use hyper;
use rand::{thread_rng, Rng};
use serde::{Serialize, Deserialize};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use tide::Request;
use std::env;

pub type Store = Arc<Mutex<HashMap<usize, usize>>>;

#[derive(Deserialize, Serialize)]
pub struct Confidence {
    pub block: usize,
    pub factor: usize,
}

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
    header: Header,
}

#[derive(Deserialize, Debug)]
pub struct Header {
    number: String,
    #[serde(rename = "extrinsicsRoot")]
    extrinsics_root: ExtrinsicsRoot,
    #[serde(rename = "parentHash")]
    parent_hash: String,
    #[serde(rename = "stateRoot")]
    state_root: String,
    digest: Digest,
}

#[derive(Deserialize, Debug)]
pub struct ExtrinsicsRoot {
    cols: u16,
    rows: u16,
    hash: String,
    commitment: Vec<u8>,
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
    block: usize,
    row: u8,
    col: u8,
    proof: Vec<u8>,
}

fn get_full_node_url() -> String {
    if let Ok(v) = env::var("FullNodeURL") {
        v
    } else {
        "http://localhost:9999".to_owned()
    }
}

pub async fn get_blockhash(block: usize) -> Result<String, String> {
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
    let client = hyper::Client::new();
    let resp = client.request(req).await.unwrap();
    let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
    let r: BlockHashResponse = serde_json::from_slice(&body).unwrap();
    Ok(r.result)
}

pub async fn get_block_by_hash(hash: String) -> Result<(), String> {
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
    let client = hyper::Client::new();
    let resp = client.request(req).await.unwrap();
    let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
    let b: BlockResponse = serde_json::from_slice(&body).unwrap();
    println!("{:?}", b);
    Ok(())
}

pub async fn get_block_by_number(block: usize) -> Result<(), String> {
    let hash = get_blockhash(block).await.unwrap();
    let _ = get_block_by_hash(hash).await.unwrap();
    Ok(())
}

pub fn get_if_confidence_available(req: &Request<Store>, block: usize) -> Result<usize, String> {
    let handle = req.state().lock().unwrap();
    if let Some(v) = handle.get(&block) {
        Ok(*v)
    } else {
        Err("not available".to_owned())
    }
}

pub fn generate_random_cells(max_rows: u8, max_cols: u8, block: usize, count: u8) -> Vec<Cell> {
    let mut rng = thread_rng();
    let mut buf = Vec::new();
    for _ in 0..count {
        let row = rng.gen::<u8>() % max_rows;
        let col = rng.gen::<u8>() % max_cols;
        buf.push(Cell{block: block, row: row, col: col, ..Default::default()});
    }
    buf
}

pub fn generate_kate_query_payload(block: usize, cells: &Vec<Cell>) -> String {
    let mut query = Vec::new();
    for cell in cells {
        query.push(format!(r#"{{"row": {}, "col": {}}}"#, cell.row, cell.col));
    }
    format!(r#"{{"id": 1, "jsonrpc": "2.0", "method": "kate_queryProof", "params": [{}, [{}]]}}"#, block, query.join(", "))
}

pub fn fill_cells_with_proofs(cells: &mut Vec<Cell>, proof: &BlockProofResponse) {
    assert_eq!(80*cells.len(), proof.result.len());
    for i in 0..cells.len() {
        let mut v = Vec::new();
        v.extend_from_slice(&proof.result[i*80..i*80+80]);
        cells[i].proof = v;
    }
}

pub async fn get_kate_proof(block: usize) -> Result<(), String> {
    let mut cells = generate_random_cells(1, 4, block, 2);
    let payload = generate_kate_query_payload(block, &cells);
    let req = hyper::Request::builder()
        .method(hyper::Method::POST)
        .uri(get_full_node_url())
        .header("Content-Type", "application/json")
        .body(hyper::Body::from(payload))
        .unwrap();
    let client = hyper::Client::new();
    let resp = client.request(req).await.unwrap();
    let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
    let proof: BlockProofResponse = serde_json::from_slice(&body).unwrap();
    fill_cells_with_proofs(&mut cells, &proof);
    println!("{:?}", cells);

    Ok(())
}
