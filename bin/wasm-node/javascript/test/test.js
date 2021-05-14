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

import * as fs from 'fs';
import * as client from "../src/index.js";
import { default as process } from 'process';

// The test will fail by default unless `process.exit(0)` is explicitly used later.
process.exitCode = 1;

// Note that the flow control below is a bit complicated and would need to be refactored if we
// add more tests.

const westendSpec = fs.readFileSync('../../westend.json', 'utf8');

(async () => {
  // Test that invalid chain specs errors are properly caught.
  await client
    .start({
      chainSpecs: ["invalid chain spec"],
    })
    .then(() => {
      console.error("Client loaded successfully despite invalid chain spec");
      process.exit(1);
    })
    .catch(() => { });

  // Basic `system_name` test.
  client
    .start({
      chainSpecs: [westendSpec],
      jsonRpcCallback: (resp, chainIndex, userData) => {
        if (resp == '{"jsonrpc":"2.0","id":1,"result":"smoldot-js"}') {
          // Test successful
          process.exit(0)
        } else {
          console.warn(resp);
          process.exit(1);
        }
      }
    })
    .then((client) => {
      client.sendJsonRpc('{"jsonrpc":"2.0","id":1,"method":"system_name","params":[]}', 0, 0);
    })
    .catch((err) => process.exit(1));
})();
