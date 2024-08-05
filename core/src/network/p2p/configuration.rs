use crate::types::{duration_seconds_format, KademliaMode};
use serde::{Deserialize, Serialize};
use std::{num::NonZeroUsize, time::Duration};

/// Libp2p AutoNAT configuration (see [RuntimeConfig] for details)
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct AutoNATConfig {
	/// Interval in which the NAT status should be re-tried if it is currently unknown or max confidence was not reached yet. (default: 10 sec)
	#[serde(with = "duration_seconds_format")]
	pub autonat_retry_interval: Duration,
	/// Interval in which the NAT should be tested again if max confidence was reached in a status. (default: 30 sec)
	#[serde(with = "duration_seconds_format")]
	pub autonat_refresh_interval: Duration,
	/// AutoNat on init delay before starting the fist probe. (default: 5 sec)
	#[serde(with = "duration_seconds_format")]
	pub autonat_boot_delay: Duration,
	/// AutoNat throttle period for re-using a peer as server for a dial-request. (default: 1 sec)
	#[serde(with = "duration_seconds_format")]
	pub autonat_throttle: Duration,
	/// Configures AutoNAT behaviour to reject probes as a server for clients that are observed at a non-global ip address (default: false)
	pub autonat_only_global_ips: bool,
}

impl Default for AutoNATConfig {
	fn default() -> Self {
		Self {
			autonat_retry_interval: Duration::from_secs(20),
			autonat_refresh_interval: Duration::from_secs(360),
			autonat_boot_delay: Duration::from_secs(5),
			autonat_throttle: Duration::from_secs(1),
			autonat_only_global_ips: false,
		}
	}
}

/// Kademlia configuration (see [RuntimeConfig] for details)
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct KademliaConfig {
	/// Kademlia configuration - WARNING: Changing the default values might cause the peer to suffer poor performance!
	/// Default Kademlia config values have been copied from rust-libp2p Kademila defaults
	///
	/// Time-to-live for DHT entries in seconds (default: 24h).
	/// Default value is set for light clients. Due to the heavy duty nature of the fat clients, it is recommended to be set far bellow this
	/// value - not greater than 1hr.
	/// Record TTL, publication and replication intervals are co-dependent, meaning that TTL >> publication_interval >> replication_interval.
	#[serde(with = "duration_seconds_format")]
	pub kad_record_ttl: Duration,
	/// Sets the (re-)publication interval of stored records in seconds. (default: 12h).
	/// Default value is set for light clients. Fat client value needs to be inferred from the TTL value.
	/// This interval should be significantly shorter than the record TTL, to ensure records do not expire prematurely.
	#[serde(with = "duration_seconds_format")]
	pub publication_interval: Duration,
	/// Sets the (re-)replication interval for stored records in seconds. (default: 3h).
	/// Default value is set for light clients. Fat client value needs to be inferred from the TTL and publication interval values.
	/// This interval should be significantly shorter than the publication interval, to ensure persistence between re-publications.
	#[serde(with = "duration_seconds_format")]
	pub record_replication_interval: Duration,
	/// The replication factor determines to how many closest peers a record is replicated. (default: 20).
	pub record_replication_factor: NonZeroUsize,
	/// Sets the allowed level of parallelism for iterative Kademlia queries. (default: 3).
	#[serde(with = "duration_seconds_format")]
	pub query_timeout: Duration,
	/// Sets the Kademlia record store pruning interval in blocks (default: 180).
	pub query_parallelism: NonZeroUsize,
	/// Sets the Kademlia caching strategy to use for successful lookups. (default: 1).
	/// If set to 0, caching is disabled.
	pub caching_max_peers: u16,
	/// Require iterative queries to use disjoint paths for increased resiliency in the presence of potentially adversarial nodes. (default: false).
	pub disjoint_query_paths: bool,
	/// The maximum number of records. (default: 2400000).
	/// The default value has been calculated to sustain ~1hr worth of cells, in case of blocks with max sizes being produces in 20s block time for fat clients
	/// (256x512) * 3 * 60
	pub max_kad_record_number: usize,
	/// The maximum size of record values, in bytes. (default: 8192).
	pub max_kad_record_size: usize,
	/// The maximum number of provider records for which the local node is the provider. (default: 1024).
	pub max_kad_provided_keys: usize,
	/// Sets Kademlia mode (server/client, default client)
	pub operation_mode: KademliaMode,
	/// Sets the automatic Kademlia server mode switch (default: true)
	pub automatic_server_mode: bool,
}

impl Default for KademliaConfig {
	fn default() -> Self {
		Self {
			kad_record_ttl: Duration::from_secs(24 * 60 * 60),
			publication_interval: Default::default(),
			record_replication_interval: Duration::from_secs(3 * 60 * 60),
			record_replication_factor: NonZeroUsize::new(5).unwrap(),
			query_timeout: Duration::from_secs(10),
			query_parallelism: NonZeroUsize::new(3).unwrap(),
			caching_max_peers: 1,
			disjoint_query_paths: false,
			max_kad_record_number: 2400000,
			max_kad_record_size: 8192,
			max_kad_provided_keys: 1024,
			operation_mode: KademliaMode::Client,
			automatic_server_mode: true,
		}
	}
}
