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
  if (!Array.isArray(config.chainSpecs))
    throw new SmoldotError('config must include a field `chainSpecs` of type Array');

  const logCallback = config.logCallback || ((level, target, message) => {
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

  // Whenever an `unsubscribeAll` is sent to the worker, the corresponding user data is pushed on
  // this array. The worker needs to send back a confirmation, which pops the first element of
  // this array. JSON-RPC responses whose user data is found in this array are silently discarded.
  // This avoids a race condition where the worker emits a JSON-RPC response while we have already
  // sent to it an `unsubscribeAll`. It is also what makes it possible for `cancelAll` to cancel
  // requests that are not subscription notifications, even while the worker only supports
  // cancelling subscriptions.
  let pendingCancelConfirmations = [];

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
      } else if (config.jsonRpcCallback) {
        if (pendingCancelConfirmations.findIndex(elem => elem == message.userData) === -1)
          config.jsonRpcCallback(message.data, message.chainIndex, message.userData);
      }

    } else if (message.kind == 'unsubscribeAllConfirmation') {
      const expected = pendingCancelConfirmations.pop();
      if (expected != message.userData)
        throw 'Unexpected unsubscribeAllConfirmation';

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
    chainSpecs: config.chainSpecs,
    // Maximum level of log entries sent by the client.
    // 0 = Logging disabled, 1 = Error, 2 = Warn, 3 = Info, 4 = Debug, 5 = Trace
    maxLogLevel: config.maxLogLevel || 5
  });

  // Initialization happens asynchronous, both because we have a worker, but also asynchronously
  // within the worker. In order to detect when initialization was successful, we perform a dummy
  // JSON-RPC request and wait for the response. While this might seem like an unnecessary
  // overhead, it is the most straight-forward solution. Any alternative with a lower overhead
  // would have a higher complexity.
  //
  // Note that using `0` for `chainIndex` means that an error will be returned if the list of
  // chain specs is empty. This is fine.
  worker.postMessage({
    ty: 'request',
    request: '{"jsonrpc":"2.0","id":1,"method":"system_name","params":[]}',
    chainIndex: 0,
    userData: 0,
  });
  pendingCancelConfirmations.push(0);
  worker.postMessage({ ty: 'unsubscribeAll', userData: 0 });

  // Now blocking until the worker sends back the response.
  // This will throw if the initialization has failed.
  await initPromise;

  return {
    sendJsonRpc: (request, chainIndex, userData) => {
      if (!workerError) {
        worker.postMessage({ ty: 'request', request, chainIndex, userData: userData || 0 });
      } else {
        throw workerError;
      }
    },
    cancelAll: (userData) => {
      if (!workerError) {
        pendingCancelConfirmations.push(userData);
        worker.postMessage({ ty: 'unsubscribeAll', userData });
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
