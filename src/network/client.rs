use std::{
	sync::Arc,
	time::{Duration, Instant},
};

use anyhow::{Context, Result};
use futures::future::join_all;
use kate_recovery::{
	config,
	data::Cell,
	matrix::{Dimensions, Position, RowIndex},
};
use libp2p::{
	kad::{record::Key, PeerRecord, Quorum, Record},
	Multiaddr, PeerId,
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, trace};

use super::Event;

#[derive(Clone)]
pub struct Client {
	cmd_sender: mpsc::Sender<Command>,
	/// Number of cells to fetch in parallel
	dht_parallelization_limit: usize,
	/// Cell time to live in DHT (in seconds)
	ttl: u64,
	/// Number of records to be put in DHT simultaneuosly
	put_batch_size: usize,
}

#[derive(Clone, Debug)]
pub struct NumSuccPut(pub usize);

struct DHTCell(Cell);

impl DHTCell {
	fn reference(&self, block: u32) -> String {
		self.0.reference(block)
	}

	fn dht_record(&self, block: u32, ttl: u64) -> Record {
		Record {
			key: self.0.reference(block).as_bytes().to_vec().into(),
			value: self.0.content.to_vec(),
			publisher: None,
			expires: Instant::now().checked_add(Duration::from_secs(ttl)),
		}
	}
}
struct DHTRow((RowIndex, Vec<u8>));

impl DHTRow {
	fn reference(&self, block: u32) -> String {
		self.0 .0.reference(block)
	}

	fn dht_record(&self, block: u32, ttl: u64) -> Record {
		Record {
			key: self.0 .0.reference(block).as_bytes().to_vec().into(),
			value: self.0 .1.clone(),
			publisher: None,
			expires: Instant::now().checked_add(Duration::from_secs(ttl)),
		}
	}
}

impl Client {
	pub fn new(
		sender: mpsc::Sender<Command>,
		dht_parallelization_limit: usize,
		ttl: u64,
		put_batch_size: usize,
	) -> Self {
		Self {
			sender,
			dht_parallelization_limit,
			ttl,
			put_batch_size,
		}
	}

	pub async fn start_listening(&self, addr: Multiaddr) -> Result<(), anyhow::Error> {
		let (sender, receiver) = oneshot::channel();
		self.sender
			.send(Command::StartListening { addr, sender })
			.await
			.context("Command receiver should not be dropped.")?;
		receiver.await.context("Sender not to be dropped.")?
	}

	pub async fn add_address(
		&self,
		peer_id: PeerId,
		peer_addr: Multiaddr,
	) -> Result<(), anyhow::Error> {
		let (sender, receiver) = oneshot::channel();
		self.sender
			.send(Command::AddAddress {
				peer_id,
				peer_addr,
				sender,
			})
			.await
			.context("Command receiver should not be dropped.")?;
		receiver.await.context("Sender not to be dropped.")?
	}

	// Events stream function creates a new stream of
	// network events and sends a command to the Event loop
	// with a required sender for event output
	pub async fn events_stream(&self) -> ReceiverStream<Event> {
		let (sender, receiver) = mpsc::channel(1000);
		self.sender
			.send(Command::Stream { sender })
			.await
			.expect("Command receiver should not be dropped.");

		ReceiverStream::new(receiver)
	}

	pub async fn bootstrap(&self, nodes: Vec<(PeerId, Multiaddr)>) -> Result<()> {
		let (sender, receiver) = oneshot::channel();
		for (peer, addr) in nodes {
			self.add_address(peer, addr.clone()).await?;
		}

		self.sender
			.send(Command::Bootstrap { sender })
			.await
			.context("Command receiver should not be dropped.")?;
		receiver.await.context("Sender not to be dropped.")?
	}

	async fn get_kad_record(&self, key: Key) -> Result<PeerRecord> {
		let (sender, receiver) = oneshot::channel();
		self.sender
			.send(Command::GetKadRecord { key, sender })
			.await
			.context("Command receiver should not be dropped.")?;
		receiver.await.context("Sender not to be dropped.")?
	}

	async fn put_kad_record_batch(&self, records: Vec<Record>, quorum: Quorum) -> NumSuccPut {
		let mut num_success: usize = 0;
		for records in records.chunks(self.put_batch_size).map(Into::into) {
			let (sender, receiver) = oneshot::channel();
			if self
				.sender
				.send(Command::PutKadRecordBatch {
					records,
					quorum,
					sender,
				})
				.await
				.context("Command receiver should not be dropped.")
				.is_err()
			{
				return NumSuccPut(num_success);
			}

			num_success +=
				if let Ok(NumSuccPut(num)) = receiver.await.context("Sender not to be dropped.") {
					num
				} else {
					num_success
				};
		}
		NumSuccPut(num_success)
	}

	// Reduces the size of Kademlias underlying hashmap
	pub async fn shrink_kademlia_map(&self) -> Result<()> {
		self.sender
			.send(Command::ReduceKademliaMapSize)
			.await
			.context("Command receiver should not be dropped.")
	}

	// Dump p2p network stats in a readable manner
	pub async fn network_stats(&self) -> Result<()> {
		self.sender
			.send(Command::NetworkObservabilityDump)
			.await
			.context("Command receiver should not be dropped.")
	}

	// Since callers ignores DHT errors, debug logs are used to observe DHT behavior.
	// Return type assumes that cell is not found in case when error is present.
	async fn fetch_cell_from_dht(&self, block_number: u32, position: Position) -> Option<Cell> {
		let reference = position.reference(block_number);
		let record_key = Key::from(reference.as_bytes().to_vec());

		trace!("Getting DHT record for reference {}", reference);

		match self.get_kad_record(record_key).await {
			Ok(peer_record) => {
				debug!("Fetched cell {reference} from the DHT");

				let try_content: Result<[u8; config::COMMITMENT_SIZE + config::CHUNK_SIZE], _> =
					peer_record.record.value.try_into();

				let Ok(content) = try_content else {
					debug!("Cannot convert cell {reference} into 80 bytes");
					return None;
				};

				Some(Cell { position, content })
			},
			Err(error) => {
				debug!("Cell {reference} not found in the DHT: {error}");
				None
			},
		}
	}

	async fn fetch_row_from_dht(
		&self,
		block_number: u32,
		row_index: u32,
	) -> Option<(u32, Vec<u8>)> {
		let row_index = RowIndex(row_index);
		let reference = row_index.reference(block_number);
		let record_key = Key::from(reference.as_bytes().to_vec());

		trace!("Getting DHT record for reference {}", reference);

		match self.get_kad_record(record_key).await {
			Ok(peer_record) => Some((row_index.0, peer_record.record.value)),
			Err(error) => {
				debug!("Row {reference} not found in the DHT: {error}");
				None
			},
		}
	}

	/// Fetches cells from DHT.
	/// Returns fetched cells and unfetched positions (so we can try RPC fetch).
	///
	/// # Arguments
	///
	/// * `block_number` - Block number
	/// * `positions` - Cell positions to fetch
	pub async fn fetch_cells_from_dht(
		&self,
		block_number: u32,
		positions: &[Position],
	) -> (Vec<Cell>, Vec<Position>) {
		let mut cells = Vec::<Option<Cell>>::with_capacity(positions.len());

		for positions in positions.chunks(self.dht_parallelization_limit) {
			let fetch = |&position| self.fetch_cell_from_dht(block_number, position);
			let results = join_all(positions.iter().map(fetch)).await;
			cells.extend(results.into_iter().collect::<Vec<_>>());
		}

		let unfetched = cells
			.iter()
			.zip(positions)
			.filter(|(cell, _)| cell.is_none())
			.map(|(_, &position)| position)
			.collect::<Vec<_>>();

		let fetched = cells.into_iter().flatten().collect();

		(fetched, unfetched)
	}

	/// Fetches rows from DHT.
	/// Returns fetched rows and unfetched row indexes (so we can try RPC fetch).
	///
	/// # Arguments
	///
	/// * `block_number` - Block number
	/// * `rows` - Row indexes to fetch
	pub async fn fetch_rows_from_dht(
		&self,
		block_number: u32,
		dimensions: Dimensions,
		row_indexes: &[u32],
	) -> Vec<Option<Vec<u8>>> {
		let mut rows = vec![None; dimensions.extended_rows() as usize];
		for row_indexes in row_indexes.chunks(self.dht_parallelization_limit) {
			let fetch = |row| self.fetch_row_from_dht(block_number, row);
			let fetched_rows = join_all(row_indexes.iter().cloned().map(fetch)).await;
			for (row_index, row) in fetched_rows.into_iter().flatten() {
				rows[row_index as usize] = Some(row);
			}
		}
		rows
	}

	async fn insert_into_dht(&self, records: Vec<(String, Record)>) -> f32 {
		if records.is_empty() {
			return 1.0;
		}
		let len = records.len() as f32;

		let num = self
			.put_kad_record_batch(records.into_iter().map(|e| e.1).collect(), Quorum::One)
			.await;

		num.0 as f32 / len
	}

	/// Inserts cells into the DHT.
	/// There is no rollback, and errors will be logged and skipped,
	/// which means that we cannot rely on error logs as alert mechanism.
	/// Returns the success rate of the PUT operations measured by dividing
	/// the number of returned errors with the total number of input cells
	///
	/// # Arguments
	///
	/// * `block` - Block number
	/// * `cells` - Matrix cells to store into DHT
	pub async fn insert_cells_into_dht(&self, block: u32, cells: Vec<Cell>) -> f32 {
		let records: Vec<_> = cells
			.into_iter()
			.map(DHTCell)
			.map(|cell| (cell.reference(block), cell.dht_record(block, self.ttl)))
			.collect::<Vec<_>>();

		self.insert_into_dht(records).await
	}

	/// Inserts rows into the DHT.
	/// There is no rollback, and errors will be logged and skipped,
	/// which means that we cannot rely on error logs as alert mechanism.
	/// Returns the success rate of the PUT operations measured by dividing
	/// the number of returned errors with the total number of input rows
	///
	/// # Arguments
	///
	/// * `block` - Block number
	/// * `rows` - Matrix rows to store into DHT
	pub async fn insert_rows_into_dht(&self, block: u32, rows: Vec<(RowIndex, Vec<u8>)>) -> f32 {
		let records: Vec<_> = rows
			.into_iter()
			.map(DHTRow)
			.map(|row| (row.reference(block), row.dht_record(block, self.ttl)))
			.collect::<Vec<_>>();

		self.insert_into_dht(records).await
	}
}

#[derive(Debug)]
pub enum Command {
	StartListening {
		addr: Multiaddr,
		sender: oneshot::Sender<Result<()>>,
	},
	AddAddress {
		peer_id: PeerId,
		peer_addr: Multiaddr,
		sender: oneshot::Sender<Result<()>>,
	},
	Stream {
		sender: mpsc::Sender<Event>,
	},
	Bootstrap {
		sender: oneshot::Sender<Result<()>>,
	},
	GetKadRecord {
		key: Key,
		sender: oneshot::Sender<Result<PeerRecord>>,
	},
	PutKadRecordBatch {
		records: Arc<[Record]>,
		quorum: Quorum,
		sender: oneshot::Sender<NumSuccPut>,
	},
	ReduceKademliaMapSize,
	NetworkObservabilityDump,
}
