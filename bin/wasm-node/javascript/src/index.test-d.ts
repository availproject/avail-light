import { expectType } from 'tsd';
import smoldot from 'smoldot';

// $expectType Promise<smoldot.SmoldotClient>
smoldot.start({
  max_log_level: 3,
  chain_spec: '',
  database_content: '',
  relay_chain_spec: '',
  json_rpc_callback: (resp) => {},
  database_save_callback: (content) => {},
});
