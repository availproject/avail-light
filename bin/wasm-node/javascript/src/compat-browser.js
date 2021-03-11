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

// Overrides `compat-nodejs.js` when in a browser.

export const net = null;
export const Worker = typeof window != 'undefined' ? window.Worker : null;
export const workerOnMessage = (worker, callback) => { worker.onmessage = (event) => callback(event.data) };
export const postMessage = (msg) => self.postMessage(msg);
export const setOnMessage = (callback) => { self.onmessage = (event) => callback(event.data) };
