# Light client for Polkadot and Substrate-based chains

This JavaScript library provides a light client for
[the Polkadot blockchain](https://polkadot.network/) and for chains built
using [the Substrate blockchain framework](https://substrate.dev/).

It is an "actual" light client, in the sense that it is byzantine-resilient.
It does not rely on the presence of an RPC server, but directly connects to
the full nodes of the network.

## Example

```
import * as smoldot from 'smoldot';

// Load a string chain specifications.
const chainSpec = Buffer.from(fs.readFileSync('./westend.json')).toString('utf8');

smoldot
  .start({
    chainSpecs: [chainSpec],
    jsonRpcCallback: (jsonRpcResponse, chainIndex, connectionId) => {
        // Called whenever the client emits a response to a JSON-RPC request,
        // or a JSON-RPC pub-sub notification.
        console.log(jsonRpcResponse)
    }
  })
  .then((client) => {
    client.sendJsonRpc('{"jsonrpc":"2.0","id":1,"method":"system_name","params":[]}', 0, 0);
  })
```

## Usage

When initializing the client with the `start` function, one must pass a list of chain
specifications corresponding to the various chains the client should try to be connected to.

The `start` function returns a `Promise` that yield a client once the chain specifications have
been successfully parsed and basic initialization is finished, but before Internet connections
are opened towards the chains.

In order to de-initialize a client, call `client.terminate()`. Any function called afterwards
will throw an exception.

After having obtained a client, use `sendJsonRpc` to send a JSON-RPC request towards the node.
The function accepts three parameters:

- A string request. See [the specification of the JSON-RPC protocol](https://www.jsonrpc.org/specification),
and [the list of requests that smoldot is capable of serving](https://polkadot.js.org/docs/substrate/rpc/).
- The index of the chain within `chainSpecs`, the list of chains passed at initialization. The
request will be performed in the context of the chosen chain.
- A `userDataId` which can be used. More information below.

If the request is well formatted, the client will send a response using the `jsonRpcCallback`
callback that was passed at initialization. This callback takes as parameter the string JSON-RPC
response, the `chainIndex`, and the `userDataId`. The `chainIndex` and `userDataId` are always
equal to the values that were passinged to `sendJsonRpc`.

If the request is a subscription, the notifications will also be sent back using the same
`jsonRpcCallback`.

All the pending requests and active subscriptions corresponding to a given `userDataId` can be
instantly cancelled by calling `client.cancelAll(userDataId)`.

The `userDataId` is opaque from the point of view of smoldot, but can be used in order to match
requests with responses. Smoldot will also attempt to distribute resources allocated to processing
JSON-RPC requests equally based on the value of `userDataId`.

## Future changes

The API described above is mostly stable. It is planned, however, in the future, to give the
possibility to add and remove chains while the client is running instead of passing a list at
initialization.

See https://github.com/paritytech/smoldot/issues/501.
