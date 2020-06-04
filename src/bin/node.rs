use core::time::Duration;
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
    env_logger::init().unwrap();

    let chain_spec = substrate_lite::chain_spec::ChainSpec::from_json_bytes(
        &include_bytes!("../../flaming-fir.json")[..],
    )
    .unwrap();

    let mut service = substrate_lite::service::ServiceBuilder::from(&chain_spec)
        .build()
        .await;

    let rpc_server = jsonrpsee::http_server(&"0.0.0.0:9933".parse().unwrap())
        .await
        .unwrap();

    let mut _foo_method = rpc_server
        .register_method(From::from("system_chain"))
        .unwrap();

    let mut informant_timer = stream::unfold((), move |_| {
        futures_timer::Delay::new(Duration::from_secs(5)).map(|_| Some(((), ())))
    })
    .map(|_| ());

    loop {
        futures::select! {
            informant = informant_timer.next() => {
                // TODO: stub
                eprintln!("{}", substrate_lite::informant::InformantLine {
                    num_connected_peers: 0,
                    best_number: 0,
                    finalized_number: 0,
                    best_hash: &"e40fbca707deed85dd9075522047d3b729aa261cc5775642b0ba43702d75ed39".parse().unwrap(),
                    finalized_hash: &"e40fbca707deed85dd9075522047d3b729aa261cc5775642b0ba43702d75ed39".parse().unwrap(),
                });
            }
            ev = service.next_event().fuse() => {
                match ev {
                    substrate_lite::service::Event::NewBlock { number, .. } => {
                        println!("new block: {}", number);
                    }
                    substrate_lite::service::Event::NewNetworkExternalAddress { address } => {
                        eprintln!("🔍 Discovered new external address for our node: {}", address);
                    }
                    _ => {}
                }
            }
        }
    }
}
