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

import { Worker, workerOnError, workerOnMessage } from './compat-nodejs.js';

export class SmoldotError extends Error {
  constructor(message) {
    super(message);
  }
}

export async function start(config) {
  if (Object.prototype.toString.call(config.chain_spec) !== '[object String]')
    throw new SmoldotError('config must include a string chain_spec');

  const logCallback = config.log_callback || ((level, target, message) => {
    if (level <= 1) {
      console.error("[" + target + "]", message);
    } else if (level == 2) {
      console.warn("[" + target + "]", message);
    } else if (level == 3) {
      console.info("[" + target + "]", message);
    } else if (level == 4) {
      console.debug("[" + target + "]", message);
    } else {
      console.trace("[" + target + "]", message);
    }
  });

  // The actual execution of Smoldot is performed in a worker thread.
  //
  // The line of code below (`new Worker(...)`) is designed to hopefully work across all
  // platforms. It should work in NodeJS, browsers, webpack
  // (https://webpack.js.org/guides/web-workers/), and parcel
  // (https://github.com/parcel-bundler/parcel/pull/5846)
  const worker = new Worker(new URL('./worker.js', import.meta.url));
  let workerError = null;

  // Build a promise that will be resolved or rejected after the initalization (that happens in
  // the worker) has finished.
  let initPromiseResolve;
  let initPromiseReject;
  const initPromise = new Promise((resolve, reject) => {
    initPromiseResolve = resolve;
    initPromiseReject = reject;
  });

  // The worker can send us messages whose type is identified through a `kind` field.
  workerOnMessage(worker, (message) => {
    if (message.kind == 'jsonrpc') {
      // If `initPromiseResolve` is non-null, then this is the initial dummy JSON-RPC request
      // that is used to determine when initialization is over. See below. It is intentionally not
      // reported with the callback.
      if (initPromiseResolve) {
        initPromiseResolve();
        initPromiseReject = null;
        initPromiseResolve = null;
      } else if (config.json_rpc_callback) {
        config.json_rpc_callback(message.data);
      }

    } else if (message.kind == 'log') {
      logCallback(message.level, message.target, message.message);

    } else {
      console.error('Unknown message type', message);
    }
  });

  workerOnError(worker, (error) => {
    // A problem happened in the worker or the smoldot Wasm VM.
    // We might still be initializing.
    if (initPromiseReject) {
      initPromiseReject(error);
      initPromiseReject = null;
      initPromiseResolve = null;
    } else {
      // This situation should only happen in case of a critical error as the result of a bug
      // somewhere. Consequently, nothing is in place to cleanly report the error if we are no
      // longer initializing.
      console.error(error);
      workerError = error;
    }
  });

  // The first message expected by the worker contains the configuration.
  worker.postMessage({
    chainSpec: config.chain_spec,
    parachainSpec: config.parachain_spec,
    // Maximum level of log entries sent by the client.
    // 0 = Logging disabled, 1 = Error, 2 = Warn, 3 = Info, 4 = Debug, 5 = Trace
    maxLogLevel: config.max_log_level || 5
  });

  // After the initialization message, all further messages expected by the worker are JSON-RPC
  // requests.

  // Initialization happens asynchronous, both because we have a worker, but also asynchronously
  // within the worker. In order to detect when initialization was successful, we perform a dummy
  // JSON-RPC request and wait for the response. While this might seem like an unnecessary
  // overhead, it is the most straight-forward solution. Any alternative with a lower overhead
  // would have a higher complexity.
  worker.postMessage('{"jsonrpc":"2.0","id":1,"method":"system_name","params":[]}')

  // Now blocking until the worker sends back the response.
  // This will throw if the initialization has failed.
  await initPromise;

  return {
    send_json_rpc: (request) => {
      if (!workerError) {
        worker.postMessage(request);
      } else {
        throw workerError;
      }
    },
    terminate: () => {
      worker.terminate();
      if (!workerError)
        workerError = new Error("terminate() has been called");
    }
  }
}
