# Light client for Polkadot and Substrate-based chains

This JavaScript library provides a light client for
[the Polkadot blockchain](https://polkadot.network/) and for chains built
using [the Substrate blockchain framework](https://substrate.dev/).

It is an "actual" light client, in the sense that it is byzantine-resilient.
It does not rely on the presence of an RPC server, but directly connects to
the full nodes of the network.

## Usage

```
import * as smoldot from 'smoldot';

// Load a string chain specifications.
const chain_spec = Buffer.from(fs.readFileSync('./westend.json')).toString('utf8');

smoldot
  .start({
    chain_spec: chain_spec,
    json_rpc_callback: (resp, chain_index) => {
        // Called whenever the client emits a response to a JSON-RPC request,
        // or a JSON-RPC pub-sub notification.
        console.log(resp)
    }
  })
  .then((client) => {
    client.send_json_rpc({
      request: '{"jsonrpc":"2.0","id":1,"method":"system_name","params":[]}',
      chain_index: 0,
    });
  })
```

The `start` function returns a `Promise`.
