// Smoldot
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

import * as smoldot from './index.js';
import { default as websocket } from 'websocket';
import * as http from 'http';
// Adjust these chain specs for the chain you want to connect to.
import { default as chain_specs } from './westend_specs.js';
import * as fs from 'fs';

let client = null;
var unsent_queue = [];
let ws_connection = null;
const database_path = 'smoldot-demo-db.json';

var database_content = null;
try {
    database_content = fs.readFileSync(database_path, 'utf8');
} catch(error) {}

smoldot.start({
    chain_spec: JSON.stringify(chain_specs()),
    database_content: database_content,
    max_log_level: 3,  // Can be increased for more verbosity
    json_rpc_callback: (resp) => {
        if (ws_connection) {
            ws_connection.sendUTF(resp);
        }
    },
    database_save_callback: (db_content) => {
        fs.writeFileSync(database_path, db_content);
    }
})
    .then((c) => {
        client = c;
        unsent_queue.forEach((m) => client.send_json_rpc(m));
        unsent_queue = [];
    })

let server = http.createServer(function (request, response) {
    response.writeHead(404);
    response.end();
});

server.listen(9944, function () {
    console.log('Server is listening on port 9944');
    console.log('Visit https://polkadot.js.org/apps/?rpc=ws%3A%2F%2F127.0.0.1%3A9944');
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
