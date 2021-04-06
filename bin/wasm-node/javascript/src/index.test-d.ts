import smoldot from 'smoldot'; // smoldot is Smoldot

// $ExpectType Promise<SmoldotClient>
const sp = smoldot.start({
  max_log_level: 3,
  chain_spec: '',
  parachain_spec: '',
  json_rpc_callback: (resp) => {},
  log_callback: (level, target, message) => {},
});

sp.then(sm => {
  // $ExpectType void
  sm.send_json_rpc('{"id":8,"jsonrpc":"2.0","method":"system_health","params":[]}');
  // $ExpectType void
  sm.terminate();
});
