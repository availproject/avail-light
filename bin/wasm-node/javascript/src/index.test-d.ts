import smoldot, { Smoldot, SmoldotClient } from 'smoldot';

// Test the export type

// smoldot;  // $ExpectType Smoldot

// Test when suppliying all options and all params to json_rpc_callback

// $ExpectType Promise<SmoldotClient>
let sp = smoldot.start({
  max_log_level: 3,
  chain_spec: '',
  parachain_spec: '',
  json_rpc_callback: (resp, chain_index, user_data) => { },
  log_callback: (level, target, message) => { },
});

// Test when not supplying optional options and optional params

// $ExpectType Promise<SmoldotClient>
sp = smoldot.start({
  chain_spec: '',
  parachain_spec: '',
  json_rpc_callback: (resp) => { },
});

sp.then(sm => {
  // $ExpectType void
  sm.send_json_rpc('{"id":8,"jsonrpc":"2.0","method":"system_health","params":[]}', 0, 0);
  // $ExpectType void
  sm.cancel_all(0);
  // $ExpectType void
  sm.terminate();
});
