use core::convert::TryFrom;

fn main() {
    futures::executor::block_on(async_main())
}

async fn async_main() {
    env_logger::init().unwrap();

    let chain_specs = substrate_lite::chain_spec::ChainSpec::from_json_bytes(&include_bytes!("../../flaming-fir.json")[..]).unwrap();

    let mut service = substrate_lite::service::builder()
        .with_wasm_runtime(
            TryFrom::try_from(&include_bytes!("../../node_runtime.compact.wasm")[..]).unwrap(),
        )
        .build();

    loop {
        let _ = service.next_event().await;
    }
}
