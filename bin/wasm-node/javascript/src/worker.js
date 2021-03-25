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

import { Buffer } from 'buffer';
import * as compat from './compat-nodejs.js';
import { default as smoldot_js_builder } from './bindings-smoldot-js.js';
import { default as wasi_builder } from './bindings-wasi.js';

import { default as wasm_base64 } from './autogen/wasm.js';

// This variable represents the state of the worker, and serves three different purposes:
//
// - At initialization, it is set to `null`.
// - Once the first message, containing the configuration, has been received from the parent, it
//   becomes an array filled with JSON-RPC requests that are received while the Wasm VM is still
//   initializing.
// - After the Wasm VM has finished initialization, contains the `WebAssembly.Instance` object.
//
let state = null;

const startInstance = async (config) => {
  const chain_spec = config.chain_spec;
  const database_content = config.database_content;
  const parachain_spec = config.parachain_spec;
  const max_log_level = config.max_log_level;

  // The actual Wasm bytecode is base64-decoded from a constant found in a different file.
  // This is suboptimal compared to using `instantiateStreaming`, but it is the most
  // cross-platform cross-bundler approach.
  let wasm_bytecode = new Uint8Array(Buffer.from(wasm_base64, 'base64'));

  // Used to bind with the smoldot-js bindings. See the `bindings-smoldot-js.js` file.
  let smoldot_js_config = {
    json_rpc_callback: (data) => {
      // `compat.postMessage` is the same as `postMessage`, but works across environments.
      compat.postMessage({ kind: 'jsonrpc', data });
    },
    database_save_callback: (data) => {
      // `compat.postMessage` is the same as `postMessage`, but works across environments.
      compat.postMessage({ kind: 'database', data });
    }
  };

  let { bindings: smoldot_js_bindings } = smoldot_js_builder(smoldot_js_config);

  // Used to bind with the Wasi bindings. See the `bindings-wasi.js` file.
  let wasi_config = {};

  // Start the Wasm virtual machine.
  // The Rust code defines a list of imports that must be fulfilled by the environment. The second
  // parameter provides their implementations.
  let result = await WebAssembly.instantiate(wasm_bytecode, {
    // The functions with the "smoldot" prefix are specific to smoldot.
    "smoldot": smoldot_js_bindings,
    // As the Rust code is compiled for wasi, some more wasi-specific imports exist.
    "wasi_snapshot_preview1": wasi_builder(wasi_config),
  });

  smoldot_js_config.instance = result.instance;
  wasi_config.instance = result.instance;

  let chain_spec_len = Buffer.byteLength(chain_spec, 'utf8');
  let chain_spec_ptr = result.instance.exports.alloc(chain_spec_len);
  Buffer.from(result.instance.exports.memory.buffer)
    .write(chain_spec, chain_spec_ptr);

  let database_len = database_content ? Buffer.byteLength(database_content, 'utf8') : 0;
  let database_ptr = (database_len != 0) ? result.instance.exports.alloc(database_len) : 0;
  if (database_len != 0) {
    Buffer.from(result.instance.exports.memory.buffer)
      .write(database_content, database_ptr);
  }

  let parachain_spec_len = parachain_spec ? Buffer.byteLength(parachain_spec, 'utf8') : 0;
  let parachain_spec_ptr = (parachain_spec_len != 0) ? result.instance.exports.alloc(parachain_spec_len) : 0;
  if (parachain_spec_len != 0) {
    Buffer.from(result.instance.exports.memory.buffer)
      .write(parachain_spec, parachain_spec_ptr);
  }

  result.instance.exports.init(
    chain_spec_ptr, chain_spec_len,
    database_ptr, database_len,
    parachain_spec_ptr, parachain_spec_len,
    max_log_level
  );

  state.forEach((json_rpc_request) => {
    let len = Buffer.byteLength(json_rpc_request, 'utf8');
    let ptr = result.instance.exports.alloc(len);
    Buffer.from(result.instance.exports.memory.buffer).write(json_rpc_request, ptr);
    result.instance.exports.json_rpc_send(ptr, len);
  });

  state = result.instance;
};

// `compat.setOnMessage` is the same as `onmessage = ...`, but works across environments.
compat.setOnMessage((message) => {
  // See the documentation of the `state` variable for information.
  if (state == null) {
    state = [];
    startInstance(message)

  } else if (Array.isArray(state)) {
    // A JSON-RPC request has been received while the Wasm VM is still initializing. Queue it
    // for when initialization is over.
    state.push(message);

  } else {
    let len = Buffer.byteLength(message, 'utf8');
    let ptr = state.exports.alloc(len);
    Buffer.from(state.exports.memory.buffer).write(message, ptr);
    state.exports.json_rpc_send(ptr, len);
  }
});
