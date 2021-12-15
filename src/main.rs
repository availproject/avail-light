extern crate confy;
extern crate rocksdb;
extern crate structopt;

use futures_util::{SinkExt, StreamExt};
use ipfs_embed::{Multiaddr, PeerId};
use num::{BigUint, FromPrimitive};
use rocksdb::{ColumnFamilyDescriptor, Options, DB};
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::thread;
use structopt::StructOpt;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

mod client;
mod consts;
mod data;
mod http;
mod proof;
mod recovery;
mod rpc;
mod sync;
mod types;

#[derive(StructOpt, Debug)]
#[structopt(
    name = "avail-light",
    about = "Light Client for Polygon Avail Blockchain",
    author = "Polygon Avail Team",
    version = "0.1.0"
)]
struct CliOpts {
    #[structopt(short = "c", long = "config", default_value = "config.yaml")]
    config: String,
}

#[tokio::main]
pub async fn main() {
    let opts = CliOpts::from_args();
    let cfg: types::RuntimeConfig = confy::load_path(opts.config).unwrap();
    println!("Using {:?}", cfg);

    // Prepare key value data store opening
    //
    // cf = column family
    let mut confidence_cf_opts = Options::default();
    confidence_cf_opts.set_max_write_buffer_number(16);
    let confidence_cf_desp =
        ColumnFamilyDescriptor::new(consts::CONFIDENCE_FACTOR_CF, confidence_cf_opts);

    let mut block_header_cf_opts = Options::default();
    block_header_cf_opts.set_max_write_buffer_number(16);
    let block_header_cf_desp =
        ColumnFamilyDescriptor::new(consts::BLOCK_HEADER_CF, block_header_cf_opts);

    let mut block_cid_cf_opts = Options::default();
    block_cid_cf_opts.set_max_write_buffer_number(16);
    let block_cid_cf_desp = ColumnFamilyDescriptor::new(consts::BLOCK_CID_CF, block_cid_cf_opts);

    let mut db_opts = Options::default();
    db_opts.create_if_missing(true);
    db_opts.create_missing_column_families(true);

    let db = Arc::new(
        DB::open_cf_descriptors(
            &db_opts,
            cfg.avail_path.clone(),
            vec![confidence_cf_desp, block_header_cf_desp, block_cid_cf_desp],
        )
        .unwrap(),
    );
    // Have access to key value data store, now this can be safely used
    // from multiple threads of execution

    // this spawns one thread of execution which runs one http server
    // for handling RPC
    let db_0 = db.clone();
    let cfg_ = cfg.clone();
    thread::spawn(move || {
        http::run_server(db_0, cfg_).unwrap();
    });

    // communication channels being established for talking to
    // ipfs backed application client
    let (block_tx, block_rx) = sync_channel::<types::ClientMsg>(1 << 7);
    let (self_info_tx, self_info_rx) = sync_channel::<(PeerId, Multiaddr)>(1);
    let (destroy_tx, destroy_rx) = sync_channel::<bool>(1);

    // this one will spawn one thread for running ipfs client, while managing data discovery
    // and reconstruction
    let db_1 = db.clone();
    let cfg_ = cfg.clone();
    thread::spawn(move || {
        client::run_client(cfg_, db_1, block_rx, self_info_tx, destroy_rx).unwrap();
    });

    if let Ok((peer_id, addrs)) = self_info_rx.recv() {
        println!("IPFS backed application client: {}\t{:?}", peer_id, addrs);
    }

    let block_header = rpc::get_chain_header(&cfg.full_node_rpc)
        .await
        .expect("failed to get latest block header of chain");

    let latest_block = hex_to_u64_block_number(block_header.number);
    let url = cfg.full_node_rpc.clone();
    let db_2 = db.clone();
    tokio::spawn(async move {
        sync::sync_block_headers(url, 0, latest_block, db_2).await;
    });

    println!("Syncing block headers from 0 to {}", latest_block);

    //tokio-tungesnite method for ws connection to substrate.
    let url = url::Url::parse(&cfg.full_node_ws).unwrap();
    let (ws_stream, _response) = connect_async(url).await.expect("Failed to connect");
    let (mut write, mut read) = ws_stream.split();

    // attempt subscription to full node block mining stream
    write
        .send(Message::Text(
            r#"{"id":1, "jsonrpc":"2.0", "method": "subscribe_newHead"}"#.to_string() + "\n",
        ))
        .await
        .unwrap();

    let _subscription_result = read.next().await.unwrap().unwrap().into_data();
    println!("Connected to Substrate Node");

    let db_3 = db.clone();
    let cf_handle_0 = db_3.cf_handle(consts::CONFIDENCE_FACTOR_CF).unwrap();
    let cf_handle_1 = db_3.cf_handle(consts::BLOCK_HEADER_CF).unwrap();

    let read_future = read.for_each(|message| async {
        let data = message.unwrap().into_data();
        match serde_json::from_slice(&data) {
            Ok(response) => {
                let resp: types::Response = response;
                let header = resp.params.result;

                // well this is in hex form as `String`
                let block_num_hex = header.number.clone();
                // now this is in `u64`
                let num = hex_to_u64_block_number(block_num_hex);

                let max_rows = header.extrinsics_root.rows;
                let max_cols = header.extrinsics_root.cols;
                let app_index = header.app_data_lookup.index.clone();
                let commitment = header.extrinsics_root.commitment.clone();

                //hyper request for getting the kate query request
                let cells = rpc::get_kate_proof(&cfg.full_node_rpc, num, max_rows, max_cols, false)
                    .await
                    .unwrap();
                println!("Verifying block {}", num);

                //hyper request for verifying the proof
                let count = proof::verify_proof(max_rows, max_cols, &cells, &commitment);
                println!(
                    "Completed {} rounds of verification for block number {} ",
                    count, num
                );

                let conf = calculate_confidence(count);
                let serialised_conf = serialised_confidence(num, conf);

                // write confidence factor into on-disk database
                db_3.put_cf(cf_handle_0, num.to_be_bytes(), count.to_be_bytes())
                    .unwrap();
                println!(
                    "block: {}, confidence: {}, serialisedConfidence {}",
                    num, conf, serialised_conf
                );

                /*note:
                The following is the part when the user have already subscribed
                to an appID and now its verifying every cell that contains the data
                */
                if !app_index.is_empty() {
                    let req_id = cfg.app_id;
                    if conf > 92.0 && req_id > 0 {
                        let req_cells =
                            rpc::get_kate_proof(&cfg.full_node_rpc, num, max_rows, max_cols, true)
                                .await
                                .unwrap();
                        println!("Verifying block :{} because APPID is given ", num);
                        //hyper request for verifying the proof
                        let count =
                            proof::verify_proof(max_rows, max_cols, &req_cells, &commitment);
                        println!(
                            "Completed {} rounds of verification for block number {} ",
                            count, num
                        );
                    }
                }

                // push latest mined block's header into column family specified
                // for keeping block headers, to be used
                // later for verifying IPFS stored data
                //
                // @note this same data store is also written to in
                // another competing thread, which syncs all block headers
                // in range [0, LATEST], where LATEST = latest block number
                // when this process started
                db_3.put_cf(
                    cf_handle_1,
                    num.to_be_bytes(),
                    serde_json::to_string(&header).unwrap().as_bytes(),
                )
                .unwrap();

                // notify ipfs-based application client
                // that newly mined block has been received
                block_tx
                    .send(types::ClientMsg {
                        num: num,
                        max_rows: max_rows,
                        max_cols: max_cols,
                    })
                    .unwrap();
            }
            Err(error) => println!("Misconstructed Header: {:?}", error),
        }
    });

    read_future.await;
    // inform ipfs-backed application client running thread
    // that it can kill self now, as process is going to die itself !
    destroy_tx.send(true).unwrap();
}

/* note:
    following are the support functions.
*/
pub fn fill_cells_with_proofs(cells: &mut Vec<types::Cell>, proof: &types::BlockProofResponse) {
    assert_eq!(80 * cells.len(), proof.result.len());
    for i in 0..cells.len() {
        let mut v = Vec::new();
        v.extend_from_slice(&proof.result[i * 80..(i + 1) * 80]);
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

fn hex_to_u64_block_number(num: String) -> u64 {
    let wo_prefix = num.trim_start_matches("0x");
    u64::from_str_radix(wo_prefix, 16).unwrap()
}
