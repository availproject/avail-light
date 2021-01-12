// Substrate-lite
// Copyright (C) 2019-2021  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

// This file launches a WebSocket server that exposes JSON-RPC functions.

import * as substrate_lite from './index.js';
import { default as websocket } from 'websocket';
import * as http from 'http';
import { default as westend_specs } from './westend_specs.js';

let client = null;
var unsent_queue = [];
let ws_connection = null;

substrate_lite.start({
    chain_spec: JSON.stringify(westend_specs()),
    json_rpc_callback: (resp) => {
        if (ws_connection) {
            console.log("Sending back:", resp.slice(0, 100) + (resp.length > 100 ? '…' : ''));
            ws_connection.sendUTF(resp);
        }
    }
})
    .then((c) => {
        client = c;
        unsent_queue.forEach((m) => client.send_json_rpc(m));
        unsent_queue = [];
    })

let server = http.createServer(function (request, response) {
    console.log((new Date()) + ' Received request for ' + request.url);
    response.writeHead(404);
    response.end();
});

server.listen(9944, function () {
    console.log((new Date()) + ' Server is listening on port 9944');
});

let wsServer = new websocket.server({
    httpServer: server,
    autoAcceptConnections: false,
});

wsServer.on('request', function (request) {
    var connection = request.accept(request.requestedProtocols[0], request.origin);
    console.log((new Date()) + ' Connection accepted.');
    ws_connection = connection;

    connection.on('message', function (message) {
        if (message.type === 'utf8') {
            console.log('Received Message:', message.utf8Data.slice(0, 100) + (message.utf8Data.length > 100 ? '…' : ''));
            if (client) {
                client.send_json_rpc(message.utf8Data);
            } else {
                unsent_queue.push(message.utf8Data);
            }
        } else {
            throw "Unsupported type: " + message.type;
        }
    });

    connection.on('close', function (reasonCode, description) {
        console.log((new Date()) + ' Peer ' + connection.remoteAddress + ' disconnected.');
        ws_connection = null;
    });
});
