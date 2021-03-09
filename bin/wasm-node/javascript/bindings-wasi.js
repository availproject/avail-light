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

//! Exports a function that provides bindings for the Wasi interface.
//!
//! These bindings can then be used by the Wasm virtual machine to invoke Wasi-related functions.
//! See <https://wasi.dev/>.
//!
//! In order to use this code, call the function passing an object, then fill the `instance` field
//! of that object with the Wasm instance.

import { Buffer } from 'buffer';
import { default as randombytes } from 'randombytes';

export default (config) => {
    // List of environment variables to feed to the Rust program. An array of strings.
    // Example usage: `let env_vars = ["RUST_BACKTRACE=1", "RUST_LOG=foo"];`
    let env_vars = [];

    // Buffers holding temporary data being written by the Rust code to respectively stdout and
    // stderr.
    let stdout_buffer = new String();
    let stderr_buffer = new String();

    return {
        // Need to fill the buffer described by `ptr` and `len` with random data.
        // This data will be used in order to generate secrets. Do not use a dummy implementation!
        random_get: (ptr, len) => {
            let bytes = randombytes(len);
            bytes.copy(Buffer.from(config.instance.exports.memory.buffer), ptr);
            return 0;
        },

        // Writing to a file descriptor is used in order to write to stdout/stderr.
        fd_write: (fd, addr, num, out_ptr) => {
            // Only stdout and stderr are open for writing.
            if (fd != 1 && fd != 2) {
                return 8;
            }

            let mem = Buffer.from(config.instance.exports.memory.buffer);

            // `fd_write` passes a buffer containing itself a list of pointers and lengths to the
            // actual buffers. See writev(2).
            let to_write = new String("");
            let total_length = 0;
            for (let i = 0; i < num; i++) {
                let buf = mem.readUInt32LE(addr + 4 * i * 2);
                let buf_len = mem.readUInt32LE(addr + 4 * (i * 2 + 1));
                to_write += mem.toString('utf8', buf, buf + buf_len);
                total_length += buf_len;
            }

            let flush_buffer = (string) => {
                // As documented in the documentation of `println!`, lines are always split by a
                // single `\n` in Rust.
                let index = string.indexOf('\n');
                if (index != -1) {
                    // Note that it is questionnable to use `console.log` from within a library.
                    // However this simply reflects the usage of `println!` in the Rust code. In
                    // other words, it is `println!` that shouldn't be used in the first place.
                    // The harm of not showing text printed with `println!` at all is greater than
                    // the harm possibly caused by accidentally leaving a `println!` in the code.
                    console.log(string.substring(0, index));
                    return string.substring(index + 1);
                } else {
                    return string;
                }
            };

            // Append the newly-written data to either `stdout_buffer` or `stderr_buffer`, and
            // print their content if necessary.
            if (fd == 1) {
                stdout_buffer += to_write;
                stdout_buffer = flush_buffer(stdout_buffer);
            } else if (fd == 2) {
                stderr_buffer += to_write;
                stderr_buffer = flush_buffer(stderr_buffer);
            }

            // Need to write in `out_ptr` how much data was "written".
            mem.writeUInt32LE(total_length, out_ptr);
            return 0;
        },

        // It's unclear how to properly implement yielding, but a no-op works fine as well.
        sched_yield: () => {
            return 0;
        },

        // Used by Rust in catastrophic situations, such as a double panic.
        proc_exit: (ret_code) => {
            if (config.onTerminated)
                config.onTerminated();
            throw new SmoldotError(`proc_exit called: ${ret_code}`);
        },

        // Return the number of environment variables and the total size of all environment
        // variables. This is called in order to initialize buffers before `environ_get`.
        environ_sizes_get: (argc_out, argv_buf_size_out) => {
            let total_len = 0;
            env_vars.forEach(e => total_len += Buffer.byteLength(e, 'utf8') + 1); // +1 for trailing \0

            let mem = Buffer.from(config.instance.exports.memory.buffer);
            mem.writeUInt32LE(env_vars.length, argc_out);
            mem.writeUInt32LE(total_len, argv_buf_size_out);
            return 0;
        },

        // Write the environment variables to the given pointers.
        // `argv` is a pointer to a buffer that must be overwritten with a list of pointers to
        // environment variables, and `argv_buf` is a pointer to a buffer where to actually store
        // the environment variables.
        // The sizes of the buffers were determined by calling `environ_sizes_get`.
        environ_get: (argv, argv_buf) => {
            let mem = Buffer.from(config.instance.exports.memory.buffer);

            let argv_pos = 0;
            let argv_buf_pos = 0;

            env_vars.forEach(env_var => {
                let env_var_len = Buffer.byteLength(e, 'utf8');

                mem.writeUInt32LE(argv_buf + argv_buf_pos, argv + argv_pos);
                argv_pos += 4;

                mem.write(env_var, argv_buf + argv_buf_pos, env_var_len, 'utf8');
                argv_buf_pos += env_var_len;
                mem.writeUInt8(0, argv_buf + argv_buf_pos);
                argv_buf_pos += 1;
            });

            return 0;
        },
    };
}
