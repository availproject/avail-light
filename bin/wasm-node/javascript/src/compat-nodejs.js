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

// This small dummy module re-exports some types from NodeJS.
//
// A rule in the `package.json` overrides it with `compat-browser.js` when in a browser.

import { parentPort } from 'worker_threads';

export { default as net } from 'net';
export { Worker } from 'worker_threads';

export const workerOnMessage = (worker, callback) => worker.on('message', callback);
export const workerOnError = (worker, callback) => worker.on('error', callback);
export const postMessage = (msg) => parentPort.postMessage(msg);
export const setOnMessage = (callback) => parentPort.on('message', callback);
