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

//! Exports a function that provides bindings for the bindings found in the Rust part of the code.
//!
//! In order to use this code, call the function passing an object, then fill the `instance` field
//! of that object with the Wasm instance.

import { Buffer } from 'buffer';
import Websocket from 'websocket';
import { default as now } from 'performance-now';
import { default as net } from './tcp-nodejs.js';

export default (config) => {
    // Used below to store the list of all connections.
    // The indices within this array are chosen by the Rust code.
    let connections = {};

    // Set to `true` once `throw` has been called.
    // As documented, after the `throw` function has been called, it is forbidden to call any
    // further function of the Wasm virtual machine. This flag is used to enforce this.
    let terminated = false;

    const terminate = () => {
        terminated = true;
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
    };

    const bindings = {
        // Must throw an error. A human-readable message can be found in the WebAssembly memory in
        // the given buffer.
        throw: (ptr, len) => {
            terminate();
            if (config.onTerminated)
                config.onTerminated();
            let message = Buffer.from(config.instance.exports.memory.buffer).toString('utf8', ptr, ptr + len);
            throw new SmoldotError(message);
        },

        // Used by the Rust side to emit a JSON-RPC response or subscription notification.
        json_rpc_respond: (ptr, len) => {
            let message = Buffer.from(config.instance.exports.memory.buffer).toString('utf8', ptr, ptr + len);
            if (config.json_rpc_callback) {
                config.json_rpc_callback(message);
            }
        },

        // Used by the Rust side to emit a log entry.
        // See also the `max_log_level` parameter in the configuration.
        log: (level, target_ptr, target_len, message_ptr, message_len) => {
            let target = Buffer.from(config.instance.exports.memory.buffer)
                .toString('utf8', target_ptr, target_ptr + target_len);
            let message = Buffer.from(config.instance.exports.memory.buffer)
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
                    if (!terminated) {
                        try {
                            config.instance.exports.timer_finished(id);
                        } catch (error) {
                            terminate();
                            if (config.onTerminated)
                                config.onTerminated();
                            throw error;
                        }
                    }
                })
            } else {
                setTimeout(() => {
                    if (!terminated) {
                        try {
                            config.instance.exports.timer_finished(id);
                        } catch (error) {
                            terminate();
                            if (config.onTerminated)
                                config.onTerminated();
                            throw error;
                        }
                    }
                }, ms)
            }
        },

        // Must set the content of the database to the given string.
        database_save: (ptr, len) => {
            if (config.database_save_callback) {
                let content = Buffer.from(config.instance.exports.memory.buffer).toString('utf8', ptr, ptr + len);
                config.database_save_callback(content);
            }
        },

        // Must create a new connection object. This implementation stores the created object in
        // `connections`.
        connection_new: (id, addr_ptr, addr_len) => {
            try {
                if (!!connections[id]) {
                    throw new SmoldotError("internal error: connection already allocated");
                }

                let addr = Buffer.from(config.instance.exports.memory.buffer)
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
                        try {
                            config.instance.exports.connection_open(id);
                        } catch (error) {
                            terminate();
                            if (config.onTerminated)
                                config.onTerminated();
                            throw error;
                        }
                    };
                    connection.onclose = () => {
                        try {
                            config.instance.exports.connection_closed(id);
                        } catch (error) {
                            terminate();
                            if (config.onTerminated)
                                config.onTerminated();
                            throw error;
                        }
                    };
                    connection.onmessage = (msg) => {
                        try {
                            let message = Buffer.from(msg.data);
                            let ptr = config.instance.exports.alloc(message.length);
                            message.copy(Buffer.from(config.instance.exports.memory.buffer), ptr);
                            config.instance.exports.connection_message(id, ptr, message.length);
                        } catch (error) {
                            terminate();
                            if (config.onTerminated)
                                config.onTerminated();
                            throw error;
                        }
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
                        try {
                            if (connection.destroyed) return;
                            config.instance.exports.connection_open(id);
                        } catch (error) {
                            terminate();
                            if (config.onTerminated)
                                config.onTerminated();
                            throw error;
                        }
                    });
                    connection.on('close', () => {
                        try {
                            if (connection.destroyed) return;
                            config.instance.exports.connection_closed(id);
                        } catch (error) {
                            terminate();
                            if (config.onTerminated)
                                config.onTerminated();
                            throw error;
                        }
                    });
                    connection.on('error', () => { });
                    connection.on('data', (message) => {
                        try {
                            if (connection.destroyed) return;
                            let ptr = config.instance.exports.alloc(message.length);
                            message.copy(Buffer.from(config.instance.exports.memory.buffer), ptr);
                            config.instance.exports.connection_message(id, ptr, message.length);
                        } catch (error) {
                            terminate();
                            if (config.onTerminated)
                                config.onTerminated();
                            throw error;
                        }
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
            let data = Buffer.from(config.instance.exports.memory.buffer).slice(ptr, ptr + len);
            let connection = connections[id];
            if (connection.send) {
                // WebSocket
                connection.send(data);
            } else {
                // TCP
                connection.write(data);
            }
        }
    };

    return {
        bindings: bindings,
        terminate: terminate,
    }
}
