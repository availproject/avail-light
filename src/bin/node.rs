fn main() {
    futures::executor::block_on(async_main())
}

async fn async_main() {
    env_logger::init().unwrap();

    let mut service = substrate_lite::service::builder().build();

    loop {
        let _ = service.next_event().await;
    }
}
