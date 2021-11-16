use crate::proof;
use crate::rpc;
use ::futures::prelude::*;
use chrono::{DateTime, Local};
use hyper::header::ACCESS_CONTROL_ALLOW_ORIGIN;
use hyper::service::Service;
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use num::{BigUint, FromPrimitive};
use regex::Regex;
use std::collections::HashMap;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Instant,
};
use tokio;

// 💡 HTTP part where handles the RPC Queries 

//service part of hyper 
struct Handler {
    store: Arc<Mutex<HashMap<u64, u32>>>,
}

impl Service<Request<Body>> for Handler {
    type Response = Response<Body>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        fn match_url(path: &str) -> Result<u64, String> {
            let re = Regex::new(r"^(/v1/confidence/(\d{1,}))$").unwrap();
            if let Some(caps) = re.captures(path) {
                if let Some(block) = caps.get(2) {
                    return Ok(block.as_str().parse::<u64>().unwrap());
                }
            }
            Err("no match found !".to_owned())
        }

        fn mk_response(s: String) -> Result<Response<Body>, hyper::Error> {
            Ok(Response::builder()
                .status(200)
                .header(ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .header("Content-Type", "application/json")
                .body(Body::from(s))
                .unwrap())
        }

        fn get_confidence(db: Arc<Mutex<HashMap<u64, u32>>>, block: u64) -> Result<u32, String> {
            let handle = db.lock().unwrap();
            if let Some(count) = handle.get(&block) {
                Ok(*count)
            } else {
                Err("not available".to_owned())
            }
        }

        fn calculate_confidence(count: u32) -> f64 {
            100f64 * (1f64 - 1f64 / 2u32.pow(count) as f64)
        }

        fn serialised_confidence(block: u64, factor: f64) -> String {
            let _block: BigUint = FromPrimitive::from_u64(block).unwrap();
            let _factor: BigUint =
                FromPrimitive::from_u64((10f64.powi(7) * factor) as u64).unwrap();
            let _shifted: BigUint = _block << 32 | _factor;
            _shifted.to_str_radix(10)
        }

        fn set_confidence(db: &mut Arc<Mutex<HashMap<u64, u32>>>, block: u64, count: u32) {
            let mut handle = db.lock().unwrap();
            handle.insert(block, count);
        }

        let local_tm: DateTime<Local> = Local::now();
        println!(
            "⚡️ {} | {} | {}",
            local_tm.to_rfc2822(),
            req.method(),
            req.uri().path()
        );

        let mut db = self.store.clone();
        Box::pin(async move {
            let res = match req.method() {
                &Method::GET => {
                    if let Ok(block_num) = match_url(req.uri().path()) {
                        let count = match get_confidence(db.clone(), block_num) {
                            Ok(count) => {
                                println!(" confidence already stored in memory 😄 for blocknumber {} ", block_num);
                                count
                            }
                            Err(_e) => {
                                let begin = Instant::now();
                                let block = rpc::get_block_by_number(block_num).await.unwrap();
                                let max_rows = block.header.extrinsics_root.rows;
                                let max_cols = block.header.extrinsics_root.cols;
                                let cells = rpc::get_kate_proof(block_num, max_rows, max_cols)
                                    .await
                                    .unwrap();
                                let count = proof::verify_proof(
                                    max_rows,
                                    max_cols,
                                    &cells,
                                    &block.header.extrinsics_root.commitment,
                                );
                                println!(
                                    "✅ Completed {} rounds of verification for #{} in {:?}\n",
                                    count,
                                    block_num,
                                    begin.elapsed()
                                );
                                set_confidence(&mut db, block_num, count);
                                count
                            }
                        };
                        let conf = calculate_confidence(count);
                        let serialised_conf = serialised_confidence(block_num, conf);
                        mk_response(
                            format!(
                                r#"{{"block": {}, "confidence": {}, "serialisedConfidence": {}}}"#,
                                block_num, conf, serialised_conf
                            )
                            .to_owned(),
                        )
                    } else {
                        let mut not_found = Response::default();
                        *not_found.status_mut() = StatusCode::NOT_FOUND;
                        not_found
                            .headers_mut()
                            .insert(ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
                        Ok(not_found)
                    }
                }
                _ => {
                    let mut not_found = Response::default();
                    *not_found.status_mut() = StatusCode::NOT_FOUND;
                    not_found
                        .headers_mut()
                        .insert(ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
                    Ok(not_found)
                }
            };
            res
        })
    }
}

struct MakeHandler {
    store: Arc<Mutex<HashMap<u64, u32>>>,
}

impl<T> Service<T> for MakeHandler {
    type Response = Handler;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: T) -> Self::Future {
        let store = self.store.clone();
        let fut = async move { Ok(Handler { store }) };
        Box::pin(fut)
    }
}

#[tokio::main]
pub async fn run_server(
    store: Arc<Mutex<HashMap<u64, u32>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = ([127, 0, 0, 1], rpc::get_port()).into();
    let server = Server::bind(&addr).serve(MakeHandler { store });
    println!("✅ Listening on http://127.0.0.1:{}", rpc::get_port());
    server.await?;
    Ok(())
}
