use std::{fmt::Display, sync::mpsc::TrySendError, time::SystemTimeError};

use derive_more::Display;
use ipfs_embed::Cid;
use thiserror::Error;

#[derive(Copy, Clone, Debug, Display)]
pub enum StoreType {
	Ipfs,
}

#[derive(Copy, Clone, Debug, Display)]
pub enum IpldField {
	Block,
	Columns,
	Rows,
	Cell,
}

#[derive(Clone, Error, Debug)]
pub enum Client {
	#[error("CID ({0}) not found in {1}")]
	CIDNotFound(Cid, StoreType),
	#[error("Block CID ({0}) not found")]
	BlockCIDNotFound(u64),
	#[error("Mismatch block on Ipld, found {0} expected {1}")]
	MismatchBlockOnIpld(i128, u64),
	#[error("Missing `{0}` on Ipld")]
	MissingFieldOnIpld(IpldField),
	#[error("Queried column {0}, though max available columns {1}")]
	MaxColumnExceeded(u16, usize),
	#[error("Queried row {0}, though max available row {1}")]
	MaxRowExceeded(u16, usize),
	#[error("Empty column {0} in CID list")]
	EmptyColumnOnCidList(u16),
	#[error("Empty row {0} in CID list")]
	EmptyRowOnCidList(u16),
	#[error("Response channel failed due to {0}")]
	ResponseChannelFailed(#[from] TrySendError<Option<Vec<u8>>>),
	#[error("Received CID {0} doesn't match host computed CID {1}")]
	ReceivedCIDMismatch(Cid, Cid),
	#[error("Block {0} not found in local storage")]
	BlockNotFound(i128),
}

#[derive(Copy, Clone, Debug, Display)]
pub enum BlockSyncCF {
	BlockHeader,
	ConfidenceFactor,
}

#[derive(Clone, Error, Debug)]
pub enum BlockSync {
	#[error("Invalid JSON header")]
	InvalidJsonHeader,
	#[error("Store cannot write the block {0} at {1} column family")]
	StoreWriteFailed(u64, BlockSyncCF),
}

#[derive(Error, Debug)]
pub enum HttpServer {
	#[error("Hyper {0:?}")]
	Hyper(#[from] hyper::http::Error),
	#[error("Failed to find entry of block {0} in confidence store")]
	MissingConfident(u64),
	#[error("Invalid of Confident")]
	InvalidConfident,
	#[error("Invalid Url")]
	InvalidUrl,
}

#[derive(Copy, Clone, Debug)]
pub enum Topic {
	Ask,
}

#[derive(Error, Debug)]
pub enum IpfsClient {
	#[error("IPFS pin {0} of data matrix failed")]
	PinFailed(IpldField),
	#[error("IPFS cannot insert the {0} of data matrix")]
	InsertFailed(IpldField),
	#[error("Gossip Message on topic `{0:?}` cannot be sent")]
	SendGossipMessage(Topic),
}

#[derive(Error, Debug)]
pub enum App {
	#[error("Client error: {0:?}")]
	Client(#[from] Client),
	#[error("Block Sync error: {0:?}")]
	BlockSync(#[from] BlockSync),
	#[error("Anyhow error: {0:?}")]
	Anyhow(#[from] anyhow::Error),
	#[error("Timer error: {0:?}")]
	Timer(#[from] SystemTimeError),
	#[error("Store error: {0:?}")]
	Store(#[from] rocksdb::Error),
	#[error("Http server error: {0}")]
	HttpServer(#[from] HttpServer),
	#[error("IPFS error: {0}")]
	Ipfs(#[from] IpfsClient),

	#[error("Cell (r:{0},c:{1}) data is not available")]
	CellDataNotAvailable(u16, u16),
}

#[inline]
pub fn into_warning<E: Display>(e: &E) { log::warn!("{}", e) }

#[inline]
pub fn into_error<E: Display>(e: &E) { log::error!("{}", e) }

/// Return Err of the expression: `return Err($expression);`.
///
/// Used as `fail!(expression)`.
#[macro_export]
macro_rules! fail {
	( $y:expr ) => {{
		return Err($y.into());
	}};
}

/// Evaluate `$x:expr` and if not true return `Err($y:expr)`.
///
/// Used as `ensure!(expression_to_ensure, expression_to_return_on_false)`.
#[macro_export]
macro_rules! ensure {
	( $x:expr, $y:expr $(,)? ) => {{
		if !$x {
			$crate::fail!($y);
		}
	}};
}
