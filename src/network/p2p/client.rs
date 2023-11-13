use anyhow::{anyhow, Context, Result};
use futures::future::join_all;
use kate_recovery::{
	config,
	data::Cell,
	matrix::{Dimensions, Position, RowIndex},
};
use libp2p::{
	kad::{PeerRecord, Quorum, Record, RecordKey},
	multiaddr::Protocol,
	Multiaddr, PeerId,
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, trace};

use super::DHTPutSuccess;

#[derive(Clone)]
pub struct Client {
	command_sender: mpsc::Sender<Command>,
	/// Number of cells to fetch in parallel
	dht_parallelization_limit: usize,
	/// Cell time to live in DHT (in seconds)
	ttl: u64,
	/// Number of records to be put in DHT simultaneously
	put_batch_size: usize,
}

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
			command_sender: sender,
			dht_parallelization_limit,
			ttl,
			put_batch_size,
		}
	}

	pub async fn start_listening(&self, addr: Multiaddr) -> Result<()> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::StartListening {
				addr,
				response_sender,
			})
			.await
			.context("Command receiver should not be dropped.")?;
		response_receiver
			.await
			.context("Sender not to be dropped.")?
	}

	pub async fn add_address(&self, peer_id: PeerId, peer_addr: Multiaddr) -> Result<()> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::AddAddress {
				peer_id,
				peer_addr,
				response_sender,
			})
			.await
			.context("Command receiver should not be dropped.")?;
		response_receiver
			.await
			.context("Sender not to be dropped.")?
	}

	pub async fn dial_peer(&self, peer_id: PeerId, peer_addr: Multiaddr) -> Result<()> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::DialAddress {
				peer_id,
				peer_addr,
				response_sender,
			})
			.await
			.context("Command receiver should not be dropped.")?;
		response_receiver
			.await
			.context("Sender not to be dropped.")?
	}

	async fn add_autonat_server(&self, peer_id: PeerId, peer_addr: Multiaddr) -> Result<()> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::AddAutonatServer {
				peer_id,
				peer_address: peer_addr,
				response_sender,
			})
			.await
			.context("Command receiver not to be dropped.")?;
		response_receiver
			.await
			.context("Sender not to be dropped.")?
	}

	pub async fn bootstrap(&self, nodes: Vec<(PeerId, Multiaddr)>) -> Result<()> {
		let (response_sender, response_receiver) = oneshot::channel();

		for (peer, addr) in nodes {
			self.dial_peer(peer, addr.clone())
				.await
				.context("Error dialing bootstrap peer")?;
			self.add_address(peer, addr.clone()).await?;
			self.add_autonat_server(peer, addr.clone()).await?;
		}

		self.command_sender
			.send(Command::Bootstrap { response_sender })
			.await
			.context("Command receiver should not be dropped.")?;
		response_receiver
			.await
			.context("Sender not to be dropped.")?
	}

	async fn get_kad_record(&self, key: RecordKey) -> Result<PeerRecord> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetKadRecord {
				key,
				response_sender,
			})
			.await
			.context("Command receiver should not be dropped.")?;
		response_receiver
			.await
			.context("Sender not to be dropped.")?
	}

	async fn put_kad_record_batch(&self, records: Vec<Record>, quorum: Quorum) -> DHTPutSuccess {
		let mut num_success: usize = 0;
		// split input batch records into chunks that will be sent consecutively
		// these chunks are defined by the config parameter [put_batch_size]
		for chunk in records.chunks(self.put_batch_size) {
			// create oneshot for each chunk, through which will success counts be sent,
			// only for the records in that chunk
			let (response_sender, response_receiver) = oneshot::channel::<DHTPutSuccess>();
			if self
				.command_sender
				.send(Command::PutKadRecordBatch {
					records: chunk.into(),
					quorum,
					response_sender,
				})
				.await
				.context("Command receiver should not be dropped.")
				.is_err()
			{
				return DHTPutSuccess::Batch(num_success);
			}
			// wait here for successfully counted put operations from this chunk
			// waiting in this manner introduces a back pressure from overwhelming the network
			// with too many possible PUT request coming in from the whole batch
			// this is the reason why input parameter vector of records is split into chunks
			if let Ok(DHTPutSuccess::Batch(num)) = response_receiver.await {
				num_success += num;
			}
		}
		DHTPutSuccess::Batch(num_success)
	}

	pub async fn count_dht_entries(&self) -> Result<usize> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::CountDHTPeers { response_sender })
			.await
			.context("Command receiver not to be dropped.")?;
		response_receiver.await.context("Sender not to be dropped.")
	}

	async fn get_multiaddress(&self) -> Result<Option<Multiaddr>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetMultiaddress { response_sender })
			.await
			.context("Command receiver not to be dropped.")?;
		response_receiver.await.context("Sender not to be dropped.")
	}

	// Reduces the size of Kademlias underlying hashmap
	pub async fn shrink_kademlia_map(&self) -> Result<()> {
		self.command_sender
			.send(Command::ReduceKademliaMapSize)
			.await
			.context("Command receiver should not be dropped.")
	}

	// Since callers ignores DHT errors, debug logs are used to observe DHT behavior.
	// Return type assumes that cell is not found in case when error is present.
	async fn fetch_cell_from_dht(&self, block_number: u32, position: Position) -> Option<Cell> {
		let reference = position.reference(block_number);
		let record_key = RecordKey::from(reference.as_bytes().to_vec());

		trace!("Getting DHT record for reference {}", reference);

		match self.get_kad_record(record_key).await {
			Ok(peer_record) => {
				trace!("Fetched cell {reference} from the DHT");

				let try_content: Result<[u8; config::COMMITMENT_SIZE + config::CHUNK_SIZE], _> =
					peer_record.record.value.try_into();

				let Ok(content) = try_content else {
					debug!("Cannot convert cell {reference} into 80 bytes");
					return None;
				};

				Some(Cell { position, content })
			},
			Err(error) => {
				trace!("Cell {reference} not found in the DHT: {error}");
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
		let record_key = RecordKey::from(reference.as_bytes().to_vec());

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
		if let DHTPutSuccess::Batch(num) = self
			.put_kad_record_batch(records.into_iter().map(|e| e.1).collect(), Quorum::One)
			.await
		{
			num as f32 / len
		} else {
			0.0
		}
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

	pub async fn get_multiaddress_and_ip(&self) -> Result<(String, String)> {
		if let Ok(Some(addr)) = self.get_multiaddress().await {
			for protocol in &addr {
				match protocol {
					Protocol::Ip4(ip) => return Ok((addr.to_string(), ip.to_string())),
					Protocol::Ip6(ip) => return Ok((addr.to_string(), ip.to_string())),
					_ => continue,
				}
			}
			return Err(anyhow!("No IP Address was present in Multiaddress"));
		}
		Err(anyhow!("No Multiaddress was present for Local Node"))
	}
}

#[derive(Debug)]
pub enum Command {
	StartListening {
		addr: Multiaddr,
		response_sender: oneshot::Sender<Result<()>>,
	},
	AddAddress {
		peer_id: PeerId,
		peer_addr: Multiaddr,
		response_sender: oneshot::Sender<Result<()>>,
	},
	DialAddress {
		peer_id: PeerId,
		peer_addr: Multiaddr,
		response_sender: oneshot::Sender<Result<()>>,
	},
	Bootstrap {
		response_sender: oneshot::Sender<Result<()>>,
	},
	GetKadRecord {
		key: RecordKey,
		response_sender: oneshot::Sender<Result<PeerRecord>>,
	},
	PutKadRecordBatch {
		records: Vec<Record>,
		quorum: Quorum,
		response_sender: oneshot::Sender<DHTPutSuccess>,
	},
	CountDHTPeers {
		response_sender: oneshot::Sender<usize>,
	},
	GetCellsInDHTPerBlock {
		response_sender: oneshot::Sender<Result<()>>,
	},
	GetMultiaddress {
		response_sender: oneshot::Sender<Option<Multiaddr>>,
	},
	ReduceKademliaMapSize,
	AddAutonatServer {
		peer_id: PeerId,
		peer_address: Multiaddr,
		response_sender: oneshot::Sender<Result<()>>,
	},
}
