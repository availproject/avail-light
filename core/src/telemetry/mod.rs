pub mod otlp;

use crate::types::Origin;

pub const ATTRIBUTE_OPERATING_MODE: &str = "operating_mode";

pub trait Value: Send + Clone {
	fn is_allowed(&self, origin: &Origin) -> bool;
}

#[derive(Debug, PartialEq)]
pub enum MetricCounter {
	Starts,
	Up,
	SessionBlocks,
	OutgoingConnectionErrors,
	IncomingConnectionErrors,
	IncomingConnections,
	EstablishedConnections,
	IncomingPutRecord,
	IncomingGetRecord,
	EventLoopEvent,
	DHTPutRecords,
	RpcConnected,
	RpcConnectionSwitched,
}

pub trait MetricName {
	fn name(&self) -> &'static str;
}

impl MetricName for MetricCounter {
	fn name(&self) -> &'static str {
		use MetricCounter::*;
		match self {
			Starts => "light.starts",
			Up => "light.up",
			SessionBlocks => "light.session_blocks",
			OutgoingConnectionErrors => "light.outgoing_connection_errors",
			IncomingConnectionErrors => "light.incoming_connection_errors",
			IncomingConnections => "light.incoming_connections",
			EstablishedConnections => "light.established_connections",
			IncomingPutRecord => "light.incoming_put_record",
			IncomingGetRecord => "light.incoming_get_record",
			EventLoopEvent => "light.event_loop_event",
			DHTPutRecords => "light.dht.put.records",
			RpcConnected => "light.rpc.connected",
			RpcConnectionSwitched => "light.rpc.connection_switched",
		}
	}
}

impl MetricCounter {
	pub fn default_values() -> Vec<MetricCounter> {
		vec![
			MetricCounter::Starts,
			MetricCounter::Up,
			MetricCounter::SessionBlocks,
			MetricCounter::OutgoingConnectionErrors,
			MetricCounter::IncomingConnectionErrors,
			MetricCounter::IncomingConnections,
			MetricCounter::EstablishedConnections,
			MetricCounter::IncomingPutRecord,
			MetricCounter::IncomingGetRecord,
			MetricCounter::EventLoopEvent,
			MetricCounter::DHTPutRecords,
			MetricCounter::RpcConnected,
			MetricCounter::RpcConnectionSwitched,
		]
	}

	fn is_allowed(&self, origin: &Origin) -> bool {
		match (origin, self) {
			(Origin::External, MetricCounter::Starts) => true,
			(Origin::External, MetricCounter::Up) => true,
			(Origin::External, _) => false,
			(Origin::Internal, MetricCounter::DHTPutRecords) => false,
			(_, _) => true,
		}
	}
}

#[derive(Clone, Debug)]
pub enum MetricValue {
	BlockHeight(u32),
	BlockConfidence(f64),
	BlockConfidenceThreshold(f64),
	BlockProcessingDelay(f64),

	DHTReplicationFactor(u16),

	DHTFetched(f64),
	DHTFetchedPercentage(f64),
	DHTFetchDuration(f64),
	DHTPutDuration(f64),
	DHTPutSuccess(f64),

	DHTConnectedPeers(usize),
	DHTQueryTimeout(u32),
	DHTPingLatency(f64),

	RPCFetched(f64),
	RPCFetchDuration(f64),
	RPCCallDuration(f64),
}

impl MetricValue {
	pub fn default_values() -> Vec<MetricValue> {
		vec![
			MetricValue::BlockHeight(0),
			MetricValue::BlockConfidence(0.0),
			MetricValue::BlockConfidenceThreshold(0.0),
			MetricValue::BlockProcessingDelay(0.0),
			MetricValue::DHTReplicationFactor(0),
			MetricValue::DHTFetched(0.0),
			MetricValue::DHTFetchedPercentage(0.0),
			MetricValue::DHTFetchDuration(0.0),
			MetricValue::DHTPutDuration(0.0),
			MetricValue::DHTPutSuccess(0.0),
			MetricValue::DHTConnectedPeers(0),
			MetricValue::DHTQueryTimeout(0),
			MetricValue::DHTPingLatency(0.0),
			MetricValue::RPCFetched(0.0),
			MetricValue::RPCFetchDuration(0.0),
			MetricValue::RPCCallDuration(0.0),
		]
	}
}

impl MetricName for MetricValue {
	fn name(&self) -> &'static str {
		use MetricValue::*;

		match self {
			BlockHeight(_) => "light.block.height",
			BlockConfidence(_) => "light.block.confidence",
			BlockConfidenceThreshold(_) => "light.block.confidence_threshold",
			BlockProcessingDelay(_) => "light.block.processing_delay",

			DHTReplicationFactor(_) => "light.dht.replication_factor",
			DHTFetched(_) => "light.dht.fetched",
			DHTFetchedPercentage(_) => "light.dht.fetched_percentage",
			DHTFetchDuration(_) => "light.dht.fetch_duration",
			DHTPutDuration(_) => "light.dht.put_duration",
			DHTPutSuccess(_) => "light.dht.put_success",

			DHTConnectedPeers(_) => "light.dht.connected_peers",
			DHTQueryTimeout(_) => "light.dht.query_timeout",
			DHTPingLatency(_) => "light.dht.ping_latency",

			RPCFetched(_) => "light.rpc.fetched",
			RPCFetchDuration(_) => "light.rpc.fetch_duration",
			RPCCallDuration(_) => "light.rpc.call_duration",
		}
	}
}

impl Value for MetricValue {
	// Metric filter for external peers
	// Only the metrics we wish to send to OTel should be in this list
	fn is_allowed(&self, origin: &Origin) -> bool {
		match origin {
			Origin::External => matches!(
				self,
				MetricValue::DHTFetchedPercentage(_) | MetricValue::BlockConfidence(_)
			),
			_ => true,
		}
	}
}
