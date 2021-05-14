import smoldot, { Smoldot, SmoldotClient } from 'smoldot';

// Test the export type

// smoldot;  // $ExpectType Smoldot

// Test when suppliying all options and all params to json_rpc_callback

// $ExpectType Promise<SmoldotClient>
let sp = smoldot.start({
  maxLogLevel: 3,
  chainSpecs: [''],
  jsonRpcCallback: (resp, chainIndex, userData) => { },
  logCallback: (level, target, message) => { },
});

// Test when not supplying optional options and optional params

// $ExpectType Promise<SmoldotClient>
sp = smoldot.start({
  chainSpecs: [''],
  jsonRpcCallback: (resp) => { },
});

sp.then(sm => {
  // $ExpectType void
  sm.sendJsonRpc('{"id":8,"jsonrpc":"2.0","method":"system_health","params":[]}', 0, 0);
  // $ExpectType void
  sm.cancelAll(0);
  // $ExpectType void
  sm.terminate();
});
