#![recursion_limit = "256"]

use core::{cmp, time::Duration};
use futures::prelude::*;

fn main() {
    futures::executor::block_on(async_main())
}

// TODO: list of RPCs
/*
    account.nextIndex
    author.submitExtrinsic
    author.submitAndWatchExtrinsic
    chain.getBlock
    chain.getHeader
    chain.subscribeNewHeads
    chain.getBlockHash
    chain.subscribeFinalizedHeads
    state.getStorage
    state.subscribeStorage
    state.getMetadata
    state.getRuntimeVersion
    system.chain
    system.properties
    system.health
*/

async fn async_main() {
    let chain_spec = substrate_lite::chain_spec::ChainSpec::from_json_bytes(
        &include_bytes!("../../polkadot.json")[..],
    )
    .unwrap();

    const APP_INFO: app_dirs::AppInfo = app_dirs::AppInfo {
        name: "substrate-lite",
        author: "tomaka17",
    };

    let db_path =
        app_dirs::app_dir(app_dirs::AppDataType::UserData, &APP_INFO, "database").unwrap();
    let database = substrate_lite::database::open(substrate_lite::database::Config {
        path: &db_path.join(chain_spec.id()),
    })
    .unwrap();
    let database = substrate_lite::database_open_match_chain_specs(database, &chain_spec).unwrap();

    // TODO: nicer formatting
    println!(
        "Database best block: #{}",
        database.best_block_number().unwrap()
    );

    let mut service = substrate_lite::service::ServiceBuilder::from(&chain_spec)
        .with_database(database)
        .build()
        .await;

    let mut rpc_server = {
        let server = substrate_lite::rpc_server::RpcServers::<(), ()>::new(Default::default());
        // TODO:
        /*server
        .spawn_ws("0.0.0.0:9944".parse().unwrap())
        .await
        .unwrap();*/
        server
    };

    let mut informant_timer = stream::unfold((), move |_| {
        futures_timer::Delay::new(Duration::from_secs(1)).map(|_| Some(((), ())))
    })
    .map(|_| ());

    let mut network_known_best = None;

    loop {
        futures::select! {
            informant = informant_timer.next() => {
                // We end the informant line with a `\r` so that it overwrites itself every time.
                // If any other line gets printed, it will overwrite the informant, and the
                // informant will then print itself below, which is a fine behaviour.
                eprint!("{}\r", substrate_lite::informant::InformantLine {
                    chain_name: chain_spec.name(),
                    max_line_width: terminal_size::terminal_size().map(|(w, _)| w.0.into()).unwrap_or(80),
                    num_network_connections: service.num_network_connections(),
                    best_number: service.best_block_number(),
                    finalized_number: service.finalized_block_number(),
                    best_hash: &service.best_block_hash(),
                    finalized_hash: &service.finalized_block_hash(),
                    network_known_best,
                });
            }
            rpc_rq = rpc_server.next_event().fuse() => {
                todo!()
            }
            ev = service.next_event().fuse() => {
                match ev {
                    substrate_lite::service::Event::NewNetworkExternalAddress { address } => {
                        eprintln!("🔍 Discovered new external address for our node: {}", address);
                    }
                    substrate_lite::service::Event::BlockAnnounceReceived { number, .. } => {
                        if let Some(curr_best) = network_known_best {
                            network_known_best = Some(cmp::max(curr_best, number));
                        } else {
                            network_known_best = Some(number);
                        }
                    },
                    _ => {}
                }
            }
        }
    }
}
