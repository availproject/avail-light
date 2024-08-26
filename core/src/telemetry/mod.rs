use crate::types::Origin;
use async_trait::async_trait;
use color_eyre::Result;
use libp2p::{kad::Mode, Multiaddr};
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
	fn name(&self, project_name: String) -> String;
}

impl MetricName for MetricCounter {
	fn name(&self, project_name: String) -> String {
		use MetricCounter::*;
		match self {
			Starts => format!("{}.{}", project_name, "light.starts"),
			Up => format!("{}.{}", project_name, "light.up"),
			SessionBlocks => format!("{}.{}", project_name, "light.session_blocks"),
			OutgoingConnectionErrors => {
				format!("{}.{}", project_name, "light.outgoing_connection_errors")
			},
			IncomingConnectionErrors => {
				format!("{}.{}", project_name, "light.incoming_connection_errors")
			},
			IncomingConnections => format!("{}.{}", project_name, "light.incoming_connections"),
			EstablishedConnections => {
				format!("{}.{}", project_name, "light.established_connections")
			},
			IncomingPutRecord => format!("{}.{}", project_name, "light.incoming_put_record"),
			IncomingGetRecord => format!("{}.{}", project_name, "light.incoming_get_record"),
			EventLoopEvent => format!("{}.{}", project_name, "light.event_loop_event"),
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
	fn name(&self, project_name: String) -> String {
		use MetricValue::*;

		match self {
			BlockHeight(_) => format!("{}.{}", project_name, "light.block.height"),
			BlockConfidence(_) => format!("{}.{}", project_name, "light.block.confidence"),
			BlockConfidenceThreshold(_) => {
				format!("{}.{}", project_name, "light.block.confidence_threshold")
			},
			BlockProcessingDelay(_) => {
				format!("{}.{}", project_name, "light.block.processing_delay")
			},

			DHTReplicationFactor(_) => {
				format!("{}.{}", project_name, "light.dht.replication_factor")
			},
			DHTFetched(_) => format!("{}.{}", project_name, "light.dht.fetched"),
			DHTFetchedPercentage(_) => {
				format!("{}.{}", project_name, "light.dht.fetched_percentage")
			},
			DHTFetchDuration(_) => format!("{}.{}", project_name, "light.dht.fetch_duration"),
			DHTPutDuration(_) => format!("{}.{}", project_name, "light.dht.put_duration"),
			DHTPutSuccess(_) => format!("{}.{}", project_name, "light.dht.put_success"),

			DHTConnectedPeers(_) => format!("{}.{}", project_name, "light.dht.connected_peers"),
			DHTQueryTimeout(_) => format!("{}.{}", project_name, "light.dht.query_timeout"),
			DHTPingLatency(_) => format!("{}.{}", project_name, "light.dht.ping_latency"),

			RPCFetched(_) => format!("{}.{}", project_name, "light.rpc.fetched"),
			RPCFetchDuration(_) => format!("{}.{}", project_name, "light.rpc.fetch_duration"),
			RPCCallDuration(_) => format!("{}.{}", project_name, "light.rpc.call_duration"),
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
	async fn count(&self, counter: MetricCounter);
	async fn record<T>(&self, value: T)
	where
		T: metric::Value + Into<Record> + Send;
	async fn flush(&self) -> Result<()>;
	async fn update_operating_mode(&self, mode: Mode);
	async fn update_multiaddress(&self, multiaddr: Multiaddr);
}
