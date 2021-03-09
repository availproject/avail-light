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
import { default as now } from 'performance-now';
import Websocket from 'websocket';
import { default as net } from './tcp-nodejs.js';
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
  const json_rpc_callback = config.json_rpc_callback;
  const database_save_callback = config.database_save_callback;
  // Maximum level of log entries sent by the client.
  // 0 = Logging disabled, 1 = Error, 2 = Warn, 3 = Info, 4 = Debug, 5 = Trace
  const max_log_level = config.max_log_level || 5;

  if (Object.prototype.toString.call(chain_spec) !== '[object String]')
    throw new SmoldotError('config must include a string chain_spec');


  // Start of the actual function body.

  var module;

  // Used below to store the list of all connections.
  // The indices within this array are chosen by the Rust code.
  let connections = {};

  // The actual Wasm bytecode is base64-decoded from a constant found in a different file.
  // This is suboptimal compared to using `instantiateStreaming`, but it is the most
  // cross-platform cross-bundler approach.
  let wasm_bytecode = new Uint8Array(Buffer.from(wasm_base64, 'base64'));

  // Set to `true` once `throw` has been called.
  // As documented, after the `throw` function has been called, it is forbidden to call any
  // further function of the Wasm virtual machine. This flag is used to enforce this.
  let has_thrown = false;

  // Used to bind with the Wasi bindings. See the `bindings-wasi.js` file.
  let wasi_config = {
    // This should ideally also clean up all resources (such as connections and active
    // timers), but it is assumed that this function isn't going to be called anyway.
    has_thrown: () => has_thrown = true,
  };

  // Start the Wasm virtual machine.
  // The Rust code defines a list of imports that must be fulfilled by the environment. The second
  // parameter provides their implementations.
  let result = await WebAssembly.instantiate(wasm_bytecode, {
    // The functions with the "smoldot" prefix are specific to smoldot.
    "smoldot": {
      // Must throw an error. A human-readable message can be found in the WebAssembly memory in
      // the given buffer.
      throw: (ptr, len) => {
        has_thrown = true;

        Object.values(connections).forEach(connection => {
          if (connection.close) {
            // WebSocket
            connection.onopen = null;
            connection.onclose = null;
            connection.onmessage = null;
            connection.onerror = null;
            connection.close();
          } else {
            // TCP
            connection.destroy();
          }
        });

        let message = Buffer.from(module.exports.memory.buffer).toString('utf8', ptr, ptr + len);
        throw new SmoldotError(message);
      },

      // Used by the Rust side to emit a JSON-RPC response or subscription notification.
      json_rpc_respond: (ptr, len) => {
        let message = Buffer.from(module.exports.memory.buffer).toString('utf8', ptr, ptr + len);
        if (json_rpc_callback) {
          json_rpc_callback(message);
        }
      },

      // Used by the Rust side to emit a log entry.
      // See also the `max_log_level` parameter in the configuration.
      log: (level, target_ptr, target_len, message_ptr, message_len) => {
        let target = Buffer.from(module.exports.memory.buffer)
          .toString('utf8', target_ptr, target_ptr + target_len);
        let message = Buffer.from(module.exports.memory.buffer)
          .toString('utf8', message_ptr, message_ptr + message_len);

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
      },

      // Must return the UNIX time in milliseconds.
      unix_time_ms: () => Date.now(),

      // Must return the value of a monotonic clock in milliseconds.
      monotonic_clock_ms: () => now(),

      // Must call `timer_finished` after the given number of milliseconds has elapsed.
      start_timer: (id, ms) => {
        // In browsers, `setTimeout` works as expected when `ms` equals 0. However, NodeJS
        // requires a minimum of 1 millisecond (if `0` is passed, it is automatically replaced
        // with `1`) and wants you to use `setImmediate` instead.
        if (ms == 0 && typeof setImmediate === "function") {
          setImmediate(() => {
            if (!has_thrown) {
              module.exports.timer_finished(id);
            }
          })
        } else {
          setTimeout(() => {
            if (!has_thrown) {
              module.exports.timer_finished(id);
            }
          }, ms)
        }
      },

      // Must set the content of the database to the given string.
      database_save: (ptr, len) => {
        if (database_save_callback) {
          let content = Buffer.from(module.exports.memory.buffer).toString('utf8', ptr, ptr + len);
          database_save_callback(content);
        }
      },

      // Must create a new connection object. This implementation stores the created object in
      // `connections`.
      connection_new: (id, addr_ptr, addr_len) => {
        try {
          if (!!connections[id]) {
            throw new SmoldotError("internal error: connection already allocated");
          }

          let addr = Buffer.from(module.exports.memory.buffer)
            .toString('utf8', addr_ptr, addr_ptr + addr_len);

          let connection;

          // Attempt to parse the multiaddress.
          // Note: peers can decide of the content of `addr`, meaning that it shouldn't be
          // trusted.
          let ws_parsed = addr.match(/^\/(ip4|ip6|dns4|dns6|dns)\/(.*?)\/tcp\/(.*?)\/(ws|wss)$/);
          let tcp_parsed = addr.match(/^\/(ip4|ip6|dns4|dns6|dns)\/(.*?)\/tcp\/(.*?)$/);

          if (ws_parsed != null) {
            let proto = 'wss';
            if (ws_parsed[4] == 'ws') {
              proto = 'ws';
            }
            if (ws_parsed[1] == 'ip6') {
              connection = new Websocket.w3cwebsocket(proto + "://[" + ws_parsed[2] + "]:" + ws_parsed[3]);
            } else {
              connection = new Websocket.w3cwebsocket(proto + "://" + ws_parsed[2] + ":" + ws_parsed[3]);
            }

            connection.binaryType = 'arraybuffer';

            connection.onopen = () => {
              module.exports.connection_open(id);
            };
            connection.onclose = () => {
              module.exports.connection_closed(id);
            };
            connection.onmessage = (msg) => {
              let message = Buffer.from(msg.data);
              let ptr = module.exports.alloc(message.length);
              message.copy(Buffer.from(module.exports.memory.buffer), ptr);
              module.exports.connection_message(id, ptr, message.length);
            };

          } else if (tcp_parsed != null) {
            if (!net) {
              // `net` module not available, most likely because we're not in NodeJS.
              return 1;
            }

            connection = net.createConnection({
              host: tcp_parsed[2],
              port: parseInt(tcp_parsed[3], 10),
            });
            connection.setNoDelay();

            connection.on('connect', () => {
              if (connection.destroyed) return;
              module.exports.connection_open(id);
            });
            connection.on('close', () => {
              if (connection.destroyed) return;
              module.exports.connection_closed(id);
            });
            connection.on('error', () => { });
            connection.on('data', (message) => {
              if (connection.destroyed) return;
              let ptr = module.exports.alloc(message.length);
              message.copy(Buffer.from(module.exports.memory.buffer), ptr);
              module.exports.connection_message(id, ptr, message.length);
            });

          } else {
            return 1;
          }

          connections[id] = connection;
          return 0;

        } catch (error) {
          return 1;
        }
      },

      // Must close and destroy the connection object.
      connection_close: (id) => {
        let connection = connections[id];
        if (connection.close) {
          // WebSocket
          connection.onopen = null;
          connection.onclose = null;
          connection.onmessage = null;
          connection.onerror = null;
          connection.close();
        } else {
          // TCP
          connection.destroy();
        }
        connections[id] = undefined;
      },

      // Must queue the data found in the WebAssembly memory at the given pointer. It is assumed
      // that this function is called only when the connection is in an open state.
      connection_send: (id, ptr, len) => {
        let data = Buffer.from(module.exports.memory.buffer).slice(ptr, ptr + len);
        let connection = connections[id];
        if (connection.send) {
          // WebSocket
          connection.send(data);
        } else {
          // TCP
          connection.write(data);
        }
      }
    },

    // As the Rust code is compiled for wasi, some more wasi-specific imports exist.
    wasi_snapshot_preview1: wasi_builder(wasi_config),
  });

  module = result.instance;
  wasi_config.instance = module;

  let chain_spec_len = Buffer.byteLength(chain_spec, 'utf8');
  let chain_spec_ptr = module.exports.alloc(chain_spec_len);
  Buffer.from(module.exports.memory.buffer)
    .write(chain_spec, chain_spec_ptr);

  let database_len = database_content ? Buffer.byteLength(database_content, 'utf8') : 0;
  let database_ptr = (database_len != 0) ? module.exports.alloc(database_len) : 0;
  if (database_len != 0) {
    Buffer.from(module.exports.memory.buffer)
      .write(database_content, database_ptr);
  }

  let relay_chain_spec_len = relay_chain_spec ? Buffer.byteLength(relay_chain_spec, 'utf8') : 0;
  let relay_chain_spec_ptr = (relay_chain_spec_len != 0) ? module.exports.alloc(relay_chain_spec_len) : 0;
  if (relay_chain_spec_len != 0) {
    Buffer.from(module.exports.memory.buffer)
      .write(relay_chain_spec, relay_chain_spec_ptr);
  }

  module.exports.init(
    chain_spec_ptr, chain_spec_len,
    database_ptr, database_len,
    relay_chain_spec_ptr, relay_chain_spec_len,
    max_log_level
  );

  return {
    send_json_rpc: (request) => {
      if (has_thrown) {
        return;
      }

      let len = Buffer.byteLength(request, 'utf8');
      let ptr = module.exports.alloc(len);
      Buffer.from(module.exports.memory.buffer).write(request, ptr);
      module.exports.json_rpc_send(ptr, len);
    }
  }
}
