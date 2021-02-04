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

import * as client from "./index.js";
import { default as process } from 'process';
import { default as westend_specs } from './westend_specs.js';

// The test will fail by default unless `process.exit(0)` is explicitly used later.
process.exitCode = 1;

client
  .start({
    chain_spec: JSON.stringify(westend_specs()),
    json_rpc_callback: (resp) => {
      if (resp == '{"jsonrpc":"2.0","id":1,"result":"smoldot"}') {
        // Test successful
        console.info('Success');
        process.exit(0);
      } else {
        console.warn(resp);
        process.exit(1);
      }
    }
  })
  .then((client) => {
    client.send_json_rpc('{"jsonrpc":"2.0","id":1,"method":"system_name","params":[]}');
  })
  .catch((err) => process.exit(1));
