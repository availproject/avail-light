// TODO: temporary binary to try the JSON-RPC server component alone

use substrate_lite::json_rpc::websocket_server;

fn main() {
    env_logger::init();
    futures::executor::block_on(async_main())
}

async fn async_main() {
    let mut server = websocket_server::WsServer::new(websocket_server::Config {
        bind_address: "0.0.0.0:9494".parse().unwrap(),
        max_frame_size: 1024 * 1024,
        send_buffer_len: 32,
        max_clean_rejected_sockets_shutdowns: 32,
        capacity: 16,
    })
    .await
    .unwrap();

    loop {
        match server.next_event().await {
            websocket_server::Event::ConnectionOpen { open, .. } => {
                open.accept(());
            }
            websocket_server::Event::ConnectionError { .. } => {}
            websocket_server::Event::TextFrame { message, .. } => {
                println!("{}", message);
            }
        }
    }
}
