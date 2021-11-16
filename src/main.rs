use futures_util::{SinkExt, StreamExt};
use num::{BigUint, FromPrimitive};
use serde_json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
mod http;
mod proof;
mod rpc;

//Main function of the Light-client which handles ws and rpc 

#[tokio::main]
pub async fn main() {
    pub type Sto = Arc<Mutex<HashMap<u64, u32>>>;
    let db: Sto = Arc::new(Mutex::new(HashMap::new()));
    let cp = db.clone();

    /* note: 
        thread for handling the RPC query
        RPC query is helpful when the block is mined before client started running. 
    */

    thread::spawn(move || {
        println!("RPC server is also running..😃");
        thread::sleep(time::Duration::from_millis(500));
        http::run_server(cp.clone()).unwrap();
    });

    let url = url::Url::parse(&rpc::get_ws_node_url()).unwrap();

    //tokio-tungesnite method for ws connection to substrate.
    let (ws_stream, _response) = connect_async(url).await.expect("Failed to connect");
    println!("Connected to Substrate Node");

    let (mut write, mut read) = ws_stream.split();

    write
        .send(Message::Text(
            r#"{
        "id":1, 
        "jsonrpc":"2.0", 
        "method": "subscribe_newHead"
    }"#
            .to_string()
                + "\n",
        ))
        .await
        .unwrap();

    let _subscription_result = read.next().await.unwrap().unwrap().into_data();

    let read_future = read.for_each(|message| async {
        let data = message.unwrap().into_data();
        match serde_json::from_slice(&data) {
            Ok(response) => {
                let response:rpc::Response = response;
                let block_number = response.params.result.number;
                let raw = &block_number;
                let without_prefix = raw.trim_start_matches("0x");
                let z = u64::from_str_radix(without_prefix, 16);
                let num = &z.unwrap();
                let max_rows = response.params.result.extrinsics_root.rows;
                let max_cols = response.params.result.extrinsics_root.cols;
                let commitment = response.params.result.extrinsics_root.commitment;

                //hyper request for getting the kate query request
                let cells = rpc::get_kate_proof(*num, max_rows, max_cols).await.unwrap();
                println!("\n🛠   Verifying block :{}", *num);

                //hyper request for verifying the proof
                let count = proof::verify_proof(max_rows, max_cols, &cells, &commitment);
                println!(
                    "✅ Completed {} rounds of verification for block number {} ",
                    count, num
                );

                let conf = calculate_confidence(count);
                let serialised_conf = serialised_confidence(*num, conf);
                let mut handle = db.lock().unwrap();
                handle.insert(*num, count);
                println!(
                    "block: {}, confidence: {}, serialisedConfidence {}",
                    *num, conf, serialised_conf
                );
            }
            Err(error) => println!("Misconstructed Header: {:?}", error),
        }
    });

    read_future.await;

}


/* note:
    following are the support functions.
*/
pub fn fill_cells_with_proofs(cells: &mut Vec<rpc::Cell>, proof: &rpc::BlockProofResponse) {
    assert_eq!(80 * cells.len(), proof.result.len());
    for i in 0..cells.len() {
        let mut v = Vec::new();
        v.extend_from_slice(&proof.result[i * 80..i * 80 + 80]);
        cells[i].proof = v;
    }
}

fn calculate_confidence(count: u32) -> f64 {
    100f64 * (1f64 - 1f64 / 2u32.pow(count) as f64)
}

fn serialised_confidence(block: u64, factor: f64) -> String {
    let _block: BigUint = FromPrimitive::from_u64(block).unwrap();
    let _factor: BigUint = FromPrimitive::from_u64((10f64.powi(7) * factor) as u64).unwrap();
    let _shifted: BigUint = _block << 32 | _factor;
    _shifted.to_str_radix(10)
}

