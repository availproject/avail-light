use std::{
	collections::{hash_map, HashMap},
	fs,
	path::Path,
	str::FromStr,
	time::Duration,
};

use anyhow::{Context, Result};
use futures::{Stream, StreamExt};
use tokio::sync::{mpsc, oneshot};

use libp2p::{
	core::{
		either::EitherTransport,
		muxing::StreamMuxerBox,
		transport,
		upgrade::{SelectUpgrade, Version},
		ConnectedPoint,
	},
	identity::{self, ed25519, Keypair},
	kad::{
		record::Key,
		store::{MemoryStore, MemoryStoreConfig},
		BootstrapOk, GetRecordOk, Kademlia, KademliaConfig, KademliaEvent, PeerRecord, PutRecordOk,
		QueryId, QueryResult, Quorum, Record,
	},
	mplex::MplexConfig,
	multiaddr::Protocol,
	noise::NoiseAuthenticated,
	pnet::{PnetConfig, PreSharedKey},
	swarm::{SwarmBuilder, SwarmEvent},
	tcp::{GenTcpConfig, TokioTcpTransport},
	yamux::YamuxConfig,
	Multiaddr, NetworkBehaviour as LibP2PBehaviour, PeerId, Swarm, Transport,
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::info;

#[derive(Debug)]
enum QueryChannel {
	GetRecord(oneshot::Sender<Result<Vec<PeerRecord>>>),
	PutRecord(oneshot::Sender<Result<()>>),
	Bootstrap(oneshot::Sender<Result<()>>),
}

#[derive(Clone)]
pub struct Client {
	sender: mpsc::Sender<Command>,
}

impl Client {
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

	pub async fn dial(&self, peer_id: PeerId, peer_addr: Multiaddr) -> Result<()> {
		let (sender, receiver) = oneshot::channel();
		self.sender
			.send(Command::Dial {
				peer_id,
				peer_addr,
				sender,
			})
			.await
			.context("Command receiver should not be dropped.")?;
		receiver.await.context("Sender not to be dropped.")?
	}

	pub async fn bootstrap(&self, nodes: Vec<(PeerId, Multiaddr)>) -> Result<()> {
		let (sender, receiver) = oneshot::channel();
		for (peer, addr) in nodes {
			self.add_address(peer, addr.clone()).await?;
			// Bootstrap clients dialing back the first peer that connects to them produces an error
			// TODO: find the cause of the error
			self.dial(peer, addr).await?;
		}

		self.sender
			.send(Command::Bootstrap { sender })
			.await
			.context("Command receiver should not be dropped.")?;
		receiver.await.context("Sender not to be dropped.")?
	}

	pub async fn get_kad_record(&self, key: Key, quorum: Quorum) -> Result<Vec<PeerRecord>> {
		let (sender, receiver) = oneshot::channel();
		self.sender
			.send(Command::GetKadRecord {
				key,
				quorum,
				sender,
			})
			.await
			.context("Command receiver should not be dropped.")?;
		receiver.await.context("Sender not to be dropped.")?
	}

	pub async fn put_kad_record(&self, record: Record, quorum: Quorum) -> Result<()> {
		let (sender, receiver) = oneshot::channel();
		self.sender
			.send(Command::PutKadRecord {
				record,
				quorum,
				sender,
			})
			.await
			.context("Command receiver should not be dropped.")?;
		receiver.await.context("Sender not to be dropped.")?
	}
}

#[derive(Debug)]
enum Command {
	StartListening {
		addr: Multiaddr,
		sender: oneshot::Sender<Result<()>>,
	},
	AddAddress {
		peer_id: PeerId,
		peer_addr: Multiaddr,
		sender: oneshot::Sender<Result<()>>,
	},
	Dial {
		peer_id: PeerId,
		peer_addr: Multiaddr,
		sender: oneshot::Sender<Result<()>>,
	},
	Bootstrap {
		sender: oneshot::Sender<Result<()>>,
	},
	GetKadRecord {
		key: Key,
		quorum: Quorum,
		sender: oneshot::Sender<Result<Vec<PeerRecord>>>,
	},
	PutKadRecord {
		record: Record,
		quorum: Quorum,
		sender: oneshot::Sender<Result<()>>,
	},
}

#[derive(LibP2PBehaviour)]
#[behaviour(out_event = "BehaviourEvent")]
struct NetworkBehaviour {
	kademlia: Kademlia<MemoryStore>,
}

#[derive(Debug)]
enum BehaviourEvent {
	Kademlia(KademliaEvent),
}

impl From<KademliaEvent> for BehaviourEvent {
	fn from(event: KademliaEvent) -> Self {
		BehaviourEvent::Kademlia(event)
	}
}

pub struct EventLoop {
	swarm: Swarm<NetworkBehaviour>,
	command_receiver: mpsc::Receiver<Command>,
	event_sender: mpsc::Sender<Event>,
	pending_dials: HashMap<PeerId, oneshot::Sender<Result<(), anyhow::Error>>>,
	pending_kad_queries: HashMap<QueryId, QueryChannel>,
	pending_kad_routing: HashMap<PeerId, oneshot::Sender<Result<()>>>,
}

#[derive(Debug)]
pub enum Event {
	ConnectionEstablished {
		peer_id: PeerId,
		endpoint: ConnectedPoint,
	},
}

impl EventLoop {
	fn new(
		swarm: Swarm<NetworkBehaviour>,
		command_receiver: mpsc::Receiver<Command>,
		event_sender: mpsc::Sender<Event>,
	) -> Self {
		Self {
			swarm,
			command_receiver,
			event_sender,
			pending_dials: Default::default(),
			pending_kad_queries: Default::default(),
			pending_kad_routing: Default::default(),
		}
	}

	pub async fn run(mut self) {
		loop {
			tokio::select! {
				event = self.swarm.next() => self.handle_event(event.expect("Swarm stream should be infinite")).await,
				command = self.command_receiver.recv() => match command {
					Some(c) => self.handle_command(c).await,
					None => (),
				},
			}
		}
	}

	async fn handle_event(&mut self, event: SwarmEvent<BehaviourEvent, std::io::Error>) {
		match event {
			SwarmEvent::Behaviour(BehaviourEvent::Kademlia(event)) => match event {
				KademliaEvent::RoutingUpdated { peer, .. } => {
					if let Some(ch) = self.pending_kad_routing.remove(&peer.into()) {
						_ = ch.send(Ok(()));
					}
				},
				KademliaEvent::OutboundQueryCompleted { id, result, .. } => match result {
					QueryResult::GetRecord(result) => match result {
						Ok(GetRecordOk { records, .. }) => {
							if let Some(QueryChannel::GetRecord(ch)) =
								self.pending_kad_queries.remove(&id.into())
							{
								_ = ch.send(Ok(records));
							}
						},
						Err(err) => {
							if let Some(QueryChannel::GetRecord(ch)) =
								self.pending_kad_queries.remove(&id.into())
							{
								_ = ch.send(Err(err.into()));
							}
						},
					},
					QueryResult::PutRecord(result) => match result {
						Ok(PutRecordOk { .. }) => {
							if let Some(QueryChannel::PutRecord(ch)) =
								self.pending_kad_queries.remove(&id.into())
							{
								_ = ch.send(Ok(()));
							}
						},
						Err(err) => {
							if let Some(QueryChannel::PutRecord(ch)) =
								self.pending_kad_queries.remove(&id.into())
							{
								_ = ch.send(Err(err.into()));
							}
						},
					},
					QueryResult::Bootstrap(result) => match result {
						Ok(BootstrapOk { .. }) => {
							if let Some(QueryChannel::Bootstrap(ch)) =
								self.pending_kad_queries.remove(&id.into())
							{
								_ = ch.send(Ok(()));
							}
						},
						Err(err) => {
							if let Some(QueryChannel::Bootstrap(ch)) =
								self.pending_kad_queries.remove(&id.into())
							{
								_ = ch.send(Err(err.into()));
							}
						},
					},
					_ => {},
				},
				_ => {},
			},
			SwarmEvent::NewListenAddr { address, .. } => {
				let local_peer_id = *self.swarm.local_peer_id();
				info!(
					"Local node is listening on {:?}",
					address.with(Protocol::P2p(local_peer_id.into()))
				);
			},
			SwarmEvent::ConnectionEstablished {
				peer_id, endpoint, ..
			} => {
				if endpoint.is_dialer() {
					if let Some(ch) = self.pending_dials.remove(&peer_id) {
						let _ = ch.send(Ok(()));
					}
				}
				// this event is of interest,
				// pass event to output event stream
				self.event_sender
					.send(Event::ConnectionEstablished { peer_id, endpoint })
					.await
					.expect("Event receiver not to be dropped.");
			},
			SwarmEvent::OutgoingConnectionError { peer_id, error } => {
				if let Some(peer_id) = peer_id {
					if let Some(ch) = self.pending_dials.remove(&peer_id) {
						_ = ch.send(Err(error.into()));
					}
				}
			},
			SwarmEvent::Dialing(peer_id) => info!("Dialing {}", peer_id),
			_ => {},
		}
	}

	async fn handle_command(&mut self, command: Command) {
		match command {
			Command::StartListening { addr, sender } => {
				_ = match self.swarm.listen_on(addr) {
					Ok(_) => sender.send(Ok(())),
					Err(e) => sender.send(Err(e.into())),
				}
			},
			Command::AddAddress {
				peer_id,
				peer_addr,
				sender,
			} => {
				self.swarm
					.behaviour_mut()
					.kademlia
					.add_address(&peer_id, peer_addr.clone());
				self.pending_kad_routing.insert(peer_id, sender);
			},
			Command::Dial {
				peer_id,
				peer_addr,
				sender,
			} => {
				// Check if peer is not already connected
				// Dialing connected peers could happen during
				// the bootstrap of the first peer in the network
				if self.swarm.is_connected(&peer_id) {
					// just skip this dial, pretend all is fine
					_ = sender.send(Ok(()));
					return;
				}

				if let hash_map::Entry::Vacant(entry) = self.pending_dials.entry(peer_id) {
					if let Err(err) = self
						.swarm
						.dial(peer_addr.with(Protocol::P2p(peer_id.into())))
					{
						_ = sender.send(Err(err.into()));
					} else {
						entry.insert(sender);
					}
				} else {
					todo!("Implement logic for peer thats already beeing dialed.");
				}
			},
			Command::Bootstrap { sender } => {
				let query_id = self
					.swarm
					.behaviour_mut()
					.kademlia
					.bootstrap()
					.expect("DHT not to be empty");

				self.pending_kad_queries
					.insert(query_id, QueryChannel::Bootstrap(sender));
			},
			Command::GetKadRecord {
				key,
				quorum,
				sender,
			} => {
				let query_id = self.swarm.behaviour_mut().kademlia.get_record(key, quorum);
				self.pending_kad_queries
					.insert(query_id, QueryChannel::GetRecord(sender));
			},
			Command::PutKadRecord {
				record,
				quorum,
				sender,
			} => {
				let query_id = self
					.swarm
					.behaviour_mut()
					.kademlia
					.put_record(record, quorum)
					.expect("No put error.");

				self.pending_kad_queries
					.insert(query_id, QueryChannel::PutRecord(sender));
			},
		}
	}
}

pub fn init(
	seed: Option<u8>,
	psk_path: &String,
) -> Result<(Client, impl Stream<Item = Event>, EventLoop)> {
	// Create a public/private key pair, either based on a seed or random
	let id_keys = match seed {
		Some(seed) => {
			let mut bytes = [0u8; 32];
			bytes[0] = seed;
			let secret_key = ed25519::SecretKey::from_bytes(&mut bytes)
				.context("Error should only appear if length is wrong.")?;
			identity::Keypair::Ed25519(secret_key.into())
		},
		None => identity::Keypair::generate_ed25519(),
	};
	let local_peer_id = id_keys.public().to_peer_id();
	info!("Local peer id: {:?}", local_peer_id);

	// try to get psk
	let psk: Option<PreSharedKey> = get_psk(psk_path)?
		.map(|text| PreSharedKey::from_str(&text))
		.transpose()?;
	// create transport
	let transport = setup_transport(id_keys, psk);

	// create swarm that manages peers and events
	let swarm = {
		let mut kad_cfg = KademliaConfig::default();
		kad_cfg.set_query_timeout(Duration::from_secs(5 * 60));
		let store_cfg = MemoryStoreConfig {
			max_records: 24000000, // ~2hrs
			max_value_bytes: 100,
			max_providers_per_key: 1,
			max_provided_keys: 100000,
		};
		let kad_store = MemoryStore::with_config(local_peer_id, store_cfg);
		let behaviour = NetworkBehaviour {
			kademlia: Kademlia::with_config(local_peer_id, kad_store, kad_cfg),
		};

		// Build the Swarm, connecting the lower transport logic with the
		// higher layer network behaviour logic
		SwarmBuilder::new(transport, behaviour, local_peer_id)
			// connection background tasks are spawned onto the tokio runtime
			.executor(Box::new(|fut| {
				tokio::spawn(fut);
			}))
			.build()
	};

	let (command_sender, command_receiver) = mpsc::channel(10000);
	let (event_sender, event_receiver) = mpsc::channel(10000);

	Ok((
		Client {
			sender: command_sender,
		},
		ReceiverStream::new(event_receiver),
		EventLoop::new(swarm, command_receiver, event_sender),
	))
}

/// Read the pre-shared key file from the given directory
fn get_psk(location: &String) -> Result<Option<String>> {
	let path = Path::new(location);
	let swarm_key_file = path.join("swarm.key");
	match fs::read_to_string(swarm_key_file) {
		Ok(text) => Ok(Some(text)),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
		Err(e) => Err(e.into()),
	}
}

fn setup_transport(
	key_pair: Keypair,
	psk: Option<PreSharedKey>,
) -> transport::Boxed<(PeerId, StreamMuxerBox)> {
	let noise = NoiseAuthenticated::xx(&key_pair).unwrap();

	let base_transport =
		TokioTcpTransport::new(GenTcpConfig::default().nodelay(true).port_reuse(false));
	let maybe_encrypted = match psk {
		Some(psk) => EitherTransport::Left(
			base_transport.and_then(move |socket, _| PnetConfig::new(psk).handshake(socket)),
		),
		None => EitherTransport::Right(base_transport),
	};

	maybe_encrypted
		.upgrade(Version::V1)
		.authenticate(noise)
		.multiplex(SelectUpgrade::new(
			YamuxConfig::default(),
			MplexConfig::new(),
		))
		.timeout(Duration::from_secs(20))
		.boxed()
}
