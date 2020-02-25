use core::convert::TryFrom;

fn main() {
    futures::executor::block_on(async_main())
}

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
        .with_wasm_runtime(
            TryFrom::try_from(&include_bytes!("../../node_runtime.compact.wasm")[..]).unwrap(),
        )
        .build()
        .await;

    let rpc_server = jsonrpsee::http_server(&"0.0.0.0:9933".parse().unwrap()).await.unwrap();

    let foo_method = rpc_server.register_method(From::from("system_chain")).unwrap();

    loop {
        let next_server_event = service.next_event().await;
        match next_server_event {
            substrate_lite::service::Event::NewChainHead(num) => {
                println!("new chain head: {}", num);
            }
            _ => {}
        }
    }
}
