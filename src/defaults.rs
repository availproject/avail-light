//! Default config values

pub fn dht_parallelization_limit() -> usize {
	20
}

pub fn query_proof_rpc_parallel_tasks() -> usize {
	20
}

pub fn default_false() -> bool {
	false
}
pub fn threshold() -> usize {
	5000
}

pub fn replication_factor() -> u16 {
	20
}

pub fn replication_interval() -> u32 {
	3 * 60 * 60
}

pub fn publication_interval() -> u32 {
	12 * 60 * 60
}

pub fn ttl() -> u64 {
	24 * 60 * 60
}

pub fn connection_idle_timeout() -> u32 {
	30
}

pub fn query_timeout() -> u32 {
	60
}

pub fn query_parallelism() -> u16 {
	3
}

pub fn caching_max_peers() -> u16 {
	1
}

pub fn max_kad_record_number() -> u64 {
	2400000
}

pub fn max_kad_record_size() -> u64 {
	100
}

pub fn max_kad_provided_keys() -> u64 {
	1024
}
