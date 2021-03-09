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
import { default as smoldot_js_builder } from './bindings-smoldot-js.js';
import { default as wasi_builder } from './bindings-wasi.js';

import { default as wasm_base64 } from './autogen/wasm.js';

export class SmoldotError extends Error {
  constructor(message) {
    super(message);
  }
}

export async function start(config) {
  // Analyzing the content of `config`.

  const chain_spec = config.chain_spec;
  const database_content = config.database_content;
  const relay_chain_spec = config.relay_chain_spec;
  // Maximum level of log entries sent by the client.
  // 0 = Logging disabled, 1 = Error, 2 = Warn, 3 = Info, 4 = Debug, 5 = Trace
  const max_log_level = config.max_log_level || 5;

  if (Object.prototype.toString.call(chain_spec) !== '[object String]')
    throw new SmoldotError('config must include a string chain_spec');


  // The actual Wasm bytecode is base64-decoded from a constant found in a different file.
  // This is suboptimal compared to using `instantiateStreaming`, but it is the most
  // cross-platform cross-bundler approach.
  let wasm_bytecode = new Uint8Array(Buffer.from(wasm_base64, 'base64'));

  // Set to `true` once `throw` has been called.
  // As documented, after the `throw` function has been called, it is forbidden to call any
  // further function of the Wasm virtual machine. This flag is used to enforce this.
  let has_thrown = false;

  // Used to bind with the smoldot-js bindings. See the `bindings-smoldot-js.js` file.
  let smoldot_js_config = {
    onTerminated: () => has_thrown = true,
    json_rpc_callback: config.json_rpc_callback,
    database_save_callback: config.database_save_callback
  };

  let { bindings: smoldot_js_bindings, terminate } = smoldot_js_builder(smoldot_js_config);

  // Used to bind with the Wasi bindings. See the `bindings-wasi.js` file.
  let wasi_config = {
    onTerminated: () => {
      has_thrown = true;
      terminate();
    },
  };

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

  let relay_chain_spec_len = relay_chain_spec ? Buffer.byteLength(relay_chain_spec, 'utf8') : 0;
  let relay_chain_spec_ptr = (relay_chain_spec_len != 0) ? result.instance.exports.alloc(relay_chain_spec_len) : 0;
  if (relay_chain_spec_len != 0) {
    Buffer.from(result.instance.exports.memory.buffer)
      .write(relay_chain_spec, relay_chain_spec_ptr);
  }

  try {
    result.instance.exports.init(
      chain_spec_ptr, chain_spec_len,
      database_ptr, database_len,
      relay_chain_spec_ptr, relay_chain_spec_len,
      max_log_level
    );
  } catch (error) {
    has_thrown = true;
    terminate();
    throw error;
  }

  return {
    send_json_rpc: (request) => {
      if (has_thrown) {
        return;
      }

      let len = Buffer.byteLength(request, 'utf8');
      let ptr = result.instance.exports.alloc(len);
      Buffer.from(result.instance.exports.memory.buffer).write(request, ptr);
      result.instance.exports.json_rpc_send(ptr, len);
    }
  }
}
