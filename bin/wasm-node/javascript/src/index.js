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

import { Worker, workerOnMessage } from './compat-nodejs.js';

export class SmoldotError extends Error {
  constructor(message) {
    super(message);
  }
}

export async function start(config) {
  if (Object.prototype.toString.call(config.chain_spec) !== '[object String]')
    throw new SmoldotError('config must include a string chain_spec');

  // The actual execution of Smoldot is performed in a worker thread.
  //
  // The line of code below (`new Worker(...)`) is designed to hopefully work across all
  // platforms. It should work in NodeJS, browsers, webpack
  // (https://webpack.js.org/guides/web-workers/), and parcel
  // (https://github.com/parcel-bundler/parcel/pull/5846)
  const worker = new Worker(new URL('./worker.js', import.meta.url));

  // The worker can send us either a database save message, or a JSON-RPC answer.
  workerOnMessage(worker, (message) => {
    if (message.kind == 'jsonrpc') {
      if (config.json_rpc_callback)
        config.json_rpc_callback(message.data);
    } else if (message.kind == 'database') {
      if (config.database_save_callback)
        config.database_save_callback(message.data);
    } else {
      console.error('Unknown message type', message);
    }
  });

  // The first message expected by the worker contains the configuration.
  worker.postMessage({
    chain_spec: config.chain_spec,
    database_content: config.database_content,
    relay_chain_spec: config.relay_chain_spec,
    // Maximum level of log entries sent by the client.
    // 0 = Logging disabled, 1 = Error, 2 = Warn, 3 = Info, 4 = Debug, 5 = Trace
    max_log_level: config.max_log_level || 5
  });

  // After the initialization message, all further messages expected by the worker are JSON-RPC
  // requests.

  return {
    send_json_rpc: (request) => {
      worker.postMessage(request);
    }
  }
}
