// Copyright (C) 2019-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use super::{Config, Event, WsServer};

use futures::io::{BufReader, BufWriter};

#[test]
fn basic_works() {
    async_std::task::block_on(async move {
        let mut server: WsServer<i32> = WsServer::new(Config {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            max_frame_size: 1024 * 1024,
            send_buffer_len: 32,
            capacity: 32,
        })
        .await
        .unwrap();

        let server_addr = server.local_addr().unwrap();

        let client_task = async_std::task::spawn(async move {
            let (mut sender, mut receiver) = {
                let mut client = {
                    let socket = async_std::net::TcpStream::connect(server_addr)
                        .await
                        .unwrap();
                    let io = BufReader::new(BufWriter::new(socket));
                    soketto::handshake::Client::new(io, "example.com", "/")
                };

                assert!(
                    matches!(client.handshake().await.unwrap(), soketto::handshake::ServerResponse::Accepted {..})
                );
                client.into_builder().finish()
            };

            sender.send_text("hello world!").await.unwrap();
            sender.flush().await.unwrap();

            let mut message = Vec::new();
            match receiver.receive_data(&mut message).await {
                Ok(soketto::Data::Text(n)) => {
                    assert_eq!(&message[..n], b"hello back");
                }
                _ => panic!(),
            }
        });

        let id = match server.next_event().await {
            Event::ConnectionOpen { .. } => server.accept(12),
            _ => panic!(),
        };

        match server.next_event().await {
            Event::TextFrame {
                connection_id,
                user_data,
                message,
            } => {
                assert_eq!(connection_id, id);
                assert_eq!(*user_data, 12);
                *user_data += 1;
                assert_eq!(message, "hello world!");
            }
            _ => panic!(),
        };

        server.queue_send(id, "hello back".to_owned());

        match server.next_event().await {
            Event::ConnectionError {
                connection_id,
                user_data,
            } => {
                assert_eq!(connection_id, id);
                assert_eq!(user_data, 13);
            }
            _ => panic!(),
        };

        client_task.await;
    });
}
