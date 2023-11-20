use super::{
	Command, CommandSender, DHTPutSuccess, EventLoopEntries, QueryChannel, SendableCommand,
};
use anyhow::{anyhow, Context, Result};
use futures::{
	future::{self, join_all},
	FutureExt, StreamExt,
};
use kate_recovery::{
	config,
	data::Cell,
	matrix::{Dimensions, Position, RowIndex},
};
use libp2p::{
	kad::{PeerRecord, Quorum, Record, RecordKey},
	multiaddr::Protocol,
	swarm::dial_opts::DialOpts,
	Multiaddr, PeerId,
};
use std::str;
use std::{
	collections::HashMap,
	time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, trace};

#[derive(Clone)]
pub struct Client {
	command_sender: CommandSender,
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

struct StartListening {
	addr: Multiaddr,
	response_sender: Option<oneshot::Sender<Result<()>>>,
}

impl Command for StartListening {
	fn run(&mut self, mut entries: EventLoopEntries) -> anyhow::Result<(), anyhow::Error> {
		_ = entries.swarm().listen_on(self.addr.clone())?;

		// send result back
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Ok(()))
			.expect("StartListening receiver dropped");
		Ok(())
	}

	fn abort(&mut self, error: anyhow::Error) {
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Err(error))
			.expect("StartListening receiver dropped");
	}
}

struct AddAddress {
	peer_id: PeerId,
	peer_addr: Multiaddr,
	response_sender: Option<oneshot::Sender<Result<()>>>,
}

impl Command for AddAddress {
	fn run(&mut self, mut entries: EventLoopEntries) -> anyhow::Result<(), anyhow::Error> {
		_ = entries
			.behavior_mut()
			.kademlia
			.add_address(&self.peer_id, self.peer_addr.clone());

		// insert response channel into Swarm Events pending map
		entries.insert_swarm_event(self.peer_id, self.response_sender.take().unwrap());
		Ok(())
	}

	fn abort(&mut self, error: anyhow::Error) {
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Err(error))
			.expect("AddAddress receiver dropped");
	}
}

struct Bootstrap {
	response_sender: Option<oneshot::Sender<Result<()>>>,
}

impl Command for Bootstrap {
	fn run(&mut self, mut entries: EventLoopEntries) -> anyhow::Result<(), anyhow::Error> {
		let query_id = entries.behavior_mut().kademlia.bootstrap()?;

		// insert response channel into KAD Queries pending map
		let response_sender = self.response_sender.take().unwrap();
		entries.insert_query(query_id, super::QueryChannel::Bootstrap(response_sender));
		Ok(())
	}

	fn abort(&mut self, error: anyhow::Error) {
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Err(error))
			.expect("Bootstrap receiver dropped");
	}
}

struct GetKadRecord {
	key: RecordKey,
	response_sender: Option<oneshot::Sender<Result<PeerRecord>>>,
}

impl Command for GetKadRecord {
	fn run(&mut self, mut entries: EventLoopEntries) -> anyhow::Result<(), anyhow::Error> {
		let query_id = entries.behavior_mut().kademlia.get_record(self.key.clone());

		// insert response channel into KAD Queries pending map
		let response_sender = self.response_sender.take().unwrap();
		entries.insert_query(query_id, super::QueryChannel::GetRecord(response_sender));
		Ok(())
	}

	fn abort(&mut self, error: anyhow::Error) {
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Err(error))
			.expect("GetKadRecord receiver dropped");
	}
}

struct PutKadRecordBatch {
	records: Vec<Record>,
	quorum: Quorum,
	response_sender: Option<oneshot::Sender<Result<DHTPutSuccess>>>,
}

impl Command for PutKadRecordBatch {
	fn run(&mut self, mut entries: EventLoopEntries) -> anyhow::Result<(), anyhow::Error> {
		// create channels to track individual PUT results needed for success count
		let (put_result_tx, put_result_rx) = mpsc::channel::<DHTPutSuccess>(self.records.len());

		// spawn new task that waits and count all successful put queries from this batch,
		// but don't block event_loop
		let response_sender = self.response_sender.take().unwrap();
		tokio::spawn(
			<ReceiverStream<DHTPutSuccess>>::from(put_result_rx)
				// consider only while receiving single successful results
				.filter(|item| future::ready(item == &DHTPutSuccess::Single))
				.count()
				.map(DHTPutSuccess::Batch)
				// send back counted successful puts
				// signal back that this chunk of records is done
				.map(|successful_puts| response_sender.send(Ok(successful_puts))),
		);

		// go record by record and dispatch put requests through KAD
		for record in self.records.clone() {
			let query_id = entries
				.behavior_mut()
				.kademlia
				.put_record(record, self.quorum)
				.expect("Unable to perform batch Kademlia PUT operation.");
			// insert response channel into KAD Queries pending map
			entries.insert_query(
				query_id,
				QueryChannel::PutRecordBatch(put_result_tx.clone()),
			);
		}
		Ok(())
	}

	fn abort(&mut self, error: anyhow::Error) {
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Err(error))
			.expect("PutKadRecordBatch receiver dropped");
	}
}

struct CountDHTPeers {
	response_sender: Option<oneshot::Sender<Result<usize>>>,
}

impl Command for CountDHTPeers {
	fn run(&mut self, mut entries: EventLoopEntries) -> anyhow::Result<(), anyhow::Error> {
		let mut total_peers: usize = 0;
		for bucket in entries.behavior_mut().kademlia.kbuckets() {
			total_peers += bucket.num_entries();
		}

		// send result back
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Ok(total_peers))
			.expect("CountDHTPeers receiver dropped");
		Ok(())
	}

	fn abort(&mut self, error: anyhow::Error) {
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Err(error))
			.expect("CountDHTPeers receiver dropped");
	}
}

struct GetCellsInDHTPerBlock {
	response_sender: Option<oneshot::Sender<Result<()>>>,
}

impl Command for GetCellsInDHTPerBlock {
	fn run(&mut self, mut entries: EventLoopEntries) -> anyhow::Result<(), anyhow::Error> {
		let mut occurrence_map = HashMap::new();
		for record in entries.behavior_mut().kademlia.store_mut().records_iter() {
			let vec_key = record.0.to_vec();
			let record_key = str::from_utf8(&vec_key);

			let (block_num, _) = record_key
				.expect("unable to cast key to string")
				.split_once(':')
				.expect("unable to split the key string");

			let count = occurrence_map.entry(block_num.to_string()).or_insert(0);
			*count += 1;
		}
		let mut sorted: Vec<(&String, &i32)> = occurrence_map.iter().collect();
		sorted.sort_by(|a, b| a.0.cmp(b.0));
		for (block_number, cell_count) in sorted {
			trace!(
				"Number of cells in DHT for block {:?}: {}",
				block_number,
				cell_count
			);
		}
		// send result back
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Ok(()))
			.expect("GetCellsInDHTPerBlock receiver dropped");
		Ok(())
	}

	fn abort(&mut self, error: anyhow::Error) {
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Err(error))
			.expect("GetCellsInDHTPerBlock receiver dropped");
	}
}

struct GetMultiaddress {
	response_sender: Option<oneshot::Sender<Result<Multiaddr>>>,
}

impl Command for GetMultiaddress {
	fn run(&mut self, mut entries: EventLoopEntries) -> anyhow::Result<(), anyhow::Error> {
		let last_address = entries
			.swarm()
			.external_addresses()
			.last()
			.ok_or_else(|| anyhow!("The last_address should exist"))?;

		// send result back
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Ok(last_address.clone()))
			.expect("GetMultiaddress receiver dropped");
		Ok(())
	}

	fn abort(&mut self, error: anyhow::Error) {
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Err(error))
			.expect("GetMultiaddress receiver dropped");
	}
}

struct ReduceKademliaMapSize {
	response_sender: Option<oneshot::Sender<Result<()>>>,
}

impl Command for ReduceKademliaMapSize {
	fn run(&mut self, mut entries: EventLoopEntries) -> anyhow::Result<(), anyhow::Error> {
		entries.behavior_mut().kademlia.store_mut().shrink_hashmap();

		// send result back
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Ok(()))
			.expect("ReduceKademliaMapSize receiver dropped");
		Ok(())
	}

	fn abort(&mut self, _: anyhow::Error) {
		// theres should be no errors from running this Command
		debug!("No possible errors for ReduceKademliaMapSize");
	}
}

struct DialPeer {
	peer_id: PeerId,
	peer_address: Multiaddr,
	response_sender: Option<oneshot::Sender<Result<()>>>,
}

impl Command for DialPeer {
	fn run(&mut self, mut entries: EventLoopEntries) -> anyhow::Result<(), anyhow::Error> {
		entries.swarm().dial(
			DialOpts::peer_id(self.peer_id)
				.addresses(vec![self.peer_address.clone()])
				.build(),
		)?;

		// insert response channel into Swarm Events pending map
		entries.insert_swarm_event(self.peer_id, self.response_sender.take().unwrap());
		Ok(())
	}

	fn abort(&mut self, error: anyhow::Error) {
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Err(error))
			.expect("DialPeer receiver dropped");
	}
}

struct AddAutonatServer {
	peer_id: PeerId,
	address: Multiaddr,
	response_sender: Option<oneshot::Sender<Result<()>>>,
}

impl Command for AddAutonatServer {
	fn run(&mut self, mut entries: EventLoopEntries) -> anyhow::Result<(), anyhow::Error> {
		entries
			.behavior_mut()
			.auto_nat
			.add_server(self.peer_id, Some(self.address.clone()));

		// send result back
		// TODO: consider what to do if this results with None
		self.response_sender
			.take()
			.unwrap()
			.send(Ok(()))
			.expect("AddAutonatServer receiver dropped");
		Ok(())
	}

	fn abort(&mut self, _: anyhow::Error) {
		// theres should be no errors from running this Command
		debug!("No possible errors for AddAutonatServer command");
	}
}

impl Client {
	pub fn new(
		sender: CommandSender,
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

	async fn execute_sync<F, T>(&self, command_with_sender: F) -> Result<T>
	where
		F: FnOnce(oneshot::Sender<Result<T>>) -> SendableCommand,
	{
		let (response_sender, response_receiver) = oneshot::channel();
		let command = command_with_sender(response_sender);
		self.command_sender
			.send(command)
			.await
			.context("receiver should not be dropped")?;
		response_receiver
			.await
			.context("sender should not be dropped")?
	}

	pub async fn start_listening(&self, addr: Multiaddr) -> Result<()> {
		self.execute_sync(|response_sender| {
			Box::new(StartListening {
				addr,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn add_address(&self, peer_id: PeerId, peer_addr: Multiaddr) -> Result<()> {
		self.execute_sync(|response_sender| {
			Box::new(AddAddress {
				peer_id,
				peer_addr,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn dial_peer(&self, peer_id: PeerId, peer_address: Multiaddr) -> Result<()> {
		self.execute_sync(|response_sender| {
			Box::new(DialPeer {
				peer_id,
				peer_address,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn bootstrap(&self) -> Result<()> {
		self.execute_sync(|response_sender| {
			Box::new(Bootstrap {
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn add_autonat_server(&self, peer_id: PeerId, address: Multiaddr) -> Result<()> {
		self.execute_sync(|response_sender| {
			Box::new(AddAutonatServer {
				peer_id,
				address,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	pub async fn bootstrap_on_startup(&self, nodes: Vec<(PeerId, Multiaddr)>) -> Result<()> {
		for (peer, addr) in nodes {
			self.dial_peer(peer, addr.clone())
				.await
				.context("Dialing Bootstrap peer failed.")?;
			self.add_address(peer, addr.clone()).await?;

			self.add_autonat_server(peer, autonat_address(addr)).await?;
		}
		self.bootstrap().await
	}

	async fn get_kad_record(&self, key: RecordKey) -> Result<PeerRecord> {
		self.execute_sync(|response_sender| {
			Box::new(GetKadRecord {
				key,
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	async fn put_kad_record_batch(
		&self,
		records: Vec<Record>,
		quorum: Quorum,
	) -> Result<DHTPutSuccess> {
		let mut num_success: usize = 0;
		// split input batch records into chunks that will be sent consecutively
		// these chunks are defined by the config parameter [put_batch_size]
		for chunk in records.chunks(self.put_batch_size) {
			// create oneshot for each chunk, through which will success counts be sent,
			// only for the records in that chunk
			match self
				.execute_sync(|response_sender| {
					Box::new(PutKadRecordBatch {
						records: chunk.into(),
						quorum,
						response_sender: Some(response_sender),
					})
				})
				.await
			{
				Ok(DHTPutSuccess::Single) => num_success += 1,
				// wait here for successfully counted put operations from this chunk
				// waiting in this manner introduces a back pressure from overwhelming the network
				// with too many possible PUT request coming in from the whole batch
				// this is the reason why input parameter vector of records is split into chunks
				Ok(DHTPutSuccess::Batch(num)) => num_success += num,
				Err(err) => return Err(err),
			};
		}
		Ok(DHTPutSuccess::Batch(num_success))
	}

	pub async fn count_dht_entries(&self) -> Result<usize> {
		self.execute_sync(|response_sender| {
			Box::new(CountDHTPeers {
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	async fn get_multiaddress(&self) -> Result<Multiaddr> {
		self.execute_sync(|response_sender| {
			Box::new(GetMultiaddress {
				response_sender: Some(response_sender),
			})
		})
		.await
	}

	// Reduces the size of Kademlias underlying hashmap
	pub async fn shrink_kademlia_map(&self) -> Result<()> {
		self.execute_sync(|response_sender| {
			Box::new(ReduceKademliaMapSize {
				response_sender: Some(response_sender),
			})
		})
		.await
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
		if let Ok(DHTPutSuccess::Batch(num)) = self
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
		let addr = self.get_multiaddress().await?;
		for protocol in &addr {
			match protocol {
				Protocol::Ip4(ip) => return Ok((addr.to_string(), ip.to_string())),
				Protocol::Ip6(ip) => return Ok((addr.to_string(), ip.to_string())),
				_ => continue,
			}
		}
		Err(anyhow!("No IP Address was present in Multiaddress"))
	}
}

/// This utility function takes the Multiaddress as parameter and searches for
/// the Protocol::Udp(_) part of it, and replaces it with a TCP variant, while
/// shifting the port number by 1 up.
///
/// Used to setup a proper Multiaddress for AutoNat servers on TCP.
fn autonat_address(addr: Multiaddr) -> Multiaddr {
	let mut stacks = addr.iter().collect::<Vec<_>>();
	// search for the first occurrence of Protocol::Udp, to replace it with Tcp variant
	stacks.iter_mut().find_map(|protocol| {
		if let Protocol::Udp(port) = protocol {
			// replace the UDP variant, moving the port number 1 forward
			*protocol = Protocol::Tcp(*port + 1);
			Some(protocol)
		} else {
			None
		}
	});

	let mut addr = Multiaddr::empty();
	for stack in stacks {
		addr.push(stack)
	}

	addr
}
