use core::convert::TryFrom;

fn main() {
    futures::executor::block_on(async_main())
}

async fn async_main() {
    env_logger::init().unwrap();

    let mut service = substrate_lite::service::builder()
        .with_wasm_runtime(TryFrom::try_from(&include_bytes!("../../node_runtime.compact.wasm")[..]).unwrap())
        .build();

    loop {
        let _ = service.next_event().await;
    }
}
