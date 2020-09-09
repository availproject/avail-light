// TODO: temporary binary to try the JSON-RPC server component alone

use substrate_lite::json_rpc::{methods, websocket_server};

fn main() {
    env_logger::init();
    futures::executor::block_on(async_main())
}

async fn async_main() {
    let chain_spec = substrate_lite::chain_spec::ChainSpec::from_json_bytes(
        &include_bytes!("../polkadot.json")[..],
    )
    .unwrap();

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
        let (connection_id, response) = match server.next_event().await {
            websocket_server::Event::ConnectionOpen { .. } => {
                server.accept(());
                continue;
            }
            websocket_server::Event::ConnectionError { .. } => continue,
            websocket_server::Event::TextFrame {
                connection_id,
                message,
                ..
            } => {
                let (request_id, call) = methods::parse_json_call(&message).expect("bad request");
                match call {
                    methods::MethodCall::chain_getBlockHash { height } => {
                        assert_eq!(height, 0);
                        let hash = substrate_lite::calculate_genesis_block_header(
                            chain_spec.genesis_storage(),
                        )
                        .hash();
                        let response =
                            methods::Response::chain_getBlockHash(methods::HashHexString(hash))
                                .to_json_response(request_id);
                        (connection_id, response)
                    }
                    methods::MethodCall::system_chain {} => {
                        let response =
                            methods::Response::system_chain(chain_spec.name().to_owned())
                                .to_json_response(request_id);
                        (connection_id, response)
                    }
                    methods::MethodCall::system_properties {} => {
                        let response = methods::Response::system_properties(
                            chain_spec
                                .properties()
                                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                                .collect(),
                        )
                        .to_json_response(request_id);
                        (connection_id, response)
                    }
                    _ => {
                        println!("unimplemented: {:?}", call);
                        continue;
                    }
                }
            }
        };

        server.queue_send(connection_id, response);
    }
}
