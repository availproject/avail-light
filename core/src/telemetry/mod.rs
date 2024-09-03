use crate::types::Origin;
use async_trait::async_trait;
use color_eyre::Result;
use libp2p::{
	kad::{Mode, QueryStats, RecordKey},
	Multiaddr,
};
use otlp::Record;

pub mod metric;
pub mod otlp;

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
		}
	}
}

impl MetricCounter {
	fn is_buffered(&self) -> bool {
		!matches!(self, MetricCounter::Starts)
	}

	fn as_last(&self) -> bool {
		matches!(self, MetricCounter::Up)
	}

	fn is_allowed(&self, origin: &Origin) -> bool {
		match (origin, self) {
			(Origin::External, MetricCounter::Starts) => true,
			(Origin::External, MetricCounter::Up) => true,
			(Origin::External, _) => false,
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

impl metric::Value for MetricValue {
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

#[async_trait]
pub trait Metrics {
	fn count(&mut self, counter: MetricCounter);
	fn record<T>(&mut self, value: T)
	where
		T: metric::Value + Into<Record> + Send;
	fn flush(&mut self) -> Result<()>;
	fn update_multiaddress(&mut self, address: Multiaddr);
	fn update_operating_mode(&mut self, mode: Mode);
	fn handle_new_put_record(&mut self, block_num: u32, records: Vec<libp2p::kad::Record>);
	fn handle_successful_put_record(
		&mut self,
		record_key: RecordKey,
		query_stats: QueryStats,
	) -> Result<()>;
	fn handle_failed_put_record(
		&mut self,
		record_key: RecordKey,
		query_stats: QueryStats,
	) -> Result<()>;
}
