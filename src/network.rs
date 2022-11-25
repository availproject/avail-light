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
		either::{EitherError, EitherTransport},
		muxing::StreamMuxerBox,
		transport,
		upgrade::Version,
		ConnectedPoint,
	},
	identify::{
		Behaviour as IdentifyBehaviour, Config as IdentifyConfig, Event as IdentifyEvent, Info,
	},
	identity::{self, ed25519, Keypair},
	kad::{
		protocol,
		record::Key,
		store::{MemoryStore, MemoryStoreConfig},
		BootstrapOk, GetRecordOk, InboundRequest, Kademlia, KademliaConfig, KademliaEvent,
		PeerRecord, PutRecordOk, QueryId, QueryResult, Quorum, Record,
	},
	mdns::{MdnsConfig, MdnsEvent, TokioMdns},
	metrics::{Metrics, Recorder},
	multiaddr::Protocol,
	noise::NoiseAuthenticated,
	ping::{self, Behaviour as PingBehaviour, Config as PingConfig, Event as PingEvent},
	pnet::{PnetConfig, PreSharedKey},
	swarm::{
		keep_alive::Behaviour as KeepAliveBehaviour, ConnectionError, SwarmBuilder, SwarmEvent,
	},
	tcp::{GenTcpConfig, TokioTcpTransport},
	yamux::YamuxConfig,
	Multiaddr, NetworkBehaviour as LibP2PBehaviour, PeerId, Swarm, Transport,
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, trace};

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
	identify: IdentifyBehaviour,
	mdns: TokioMdns,
	ping: PingBehaviour,
	keep_alive: KeepAliveBehaviour,
}

#[derive(Debug)]
enum BehaviourEvent {
	Kademlia(KademliaEvent),
	Identify(IdentifyEvent),
	Mdns(MdnsEvent),
	Ping(PingEvent),
	Void,
}

impl From<KademliaEvent> for BehaviourEvent {
	fn from(event: KademliaEvent) -> Self {
		BehaviourEvent::Kademlia(event)
	}
}

impl From<IdentifyEvent> for BehaviourEvent {
	fn from(event: IdentifyEvent) -> Self {
		BehaviourEvent::Identify(event)
	}
}

impl From<MdnsEvent> for BehaviourEvent {
	fn from(event: MdnsEvent) -> Self {
		BehaviourEvent::Mdns(event)
	}
}

impl From<PingEvent> for BehaviourEvent {
	fn from(event: PingEvent) -> Self {
		BehaviourEvent::Ping(event)
	}
}

impl From<void::Void> for BehaviourEvent {
	fn from(_: void::Void) -> Self {
		BehaviourEvent::Void
	}
}

pub struct EventLoop {
	swarm: Swarm<NetworkBehaviour>,
	command_receiver: mpsc::Receiver<Command>,
	event_sender: mpsc::Sender<Event>,
	pending_dials: HashMap<PeerId, oneshot::Sender<Result<(), anyhow::Error>>>,
	pending_kad_queries: HashMap<QueryId, QueryChannel>,
	pending_kad_routing: HashMap<PeerId, oneshot::Sender<Result<()>>>,
	metrics: Metrics,
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
		metrics: Metrics,
	) -> Self {
		Self {
			swarm,
			command_receiver,
			event_sender,
			pending_dials: Default::default(),
			pending_kad_queries: Default::default(),
			pending_kad_routing: Default::default(),
			metrics,
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

	async fn handle_event(
		&mut self,
		event: SwarmEvent<
			BehaviourEvent,
			EitherError<
				EitherError<
					EitherError<EitherError<std::io::Error, std::io::Error>, void::Void>,
					ping::Failure,
				>,
				void::Void,
			>,
		>,
	) {
		match event {
			SwarmEvent::Behaviour(BehaviourEvent::Kademlia(event)) => {
				// record KAD Behaviour events
				self.metrics.record(&event);

				match event {
					KademliaEvent::RoutingUpdated {
						peer,
						is_new_peer,
						addresses,
						old_peer,
						..
					} => {
						debug!("Routing updated. Peer: {:?}. is_new_peer: {:?}. Addresses: {:?}. Old peer: {:?}", peer, is_new_peer, addresses, old_peer);
						if let Some(ch) = self.pending_kad_routing.remove(&peer.into()) {
							_ = ch.send(Ok(()));
						}
					},
					KademliaEvent::RoutablePeer { peer, address } => {
						debug!("RoutablePeer. Peer: {:?}.  Address: {:?}", peer, address);
					},
					KademliaEvent::UnroutablePeer { peer } => {
						debug!("UnroutablePeer. Peer: {:?}", peer);
					},
					KademliaEvent::PendingRoutablePeer { peer, address } => {
						debug!(
							"Pending routablePeer. Peer: {:?}.  Address: {:?}",
							peer, address
						);
					},
					KademliaEvent::InboundRequest { request } => {
						trace!("Inbound request: {:?}", request);
						if let InboundRequest::PutRecord { source, record, .. } = request {
							if let Some(block_ref) = record {
								trace!(
									"Inbound PUT request record key: {:?}. Source: {:?}",
									block_ref.key,
									source
								);
							}
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
							Ok(BootstrapOk {
								peer,
								num_remaining,
							}) => {
								debug!(
									"BootstrapOK event. PeerID: {:?}. Num remaining: {:?}.",
									peer, num_remaining
								);
								if let Some(QueryChannel::Bootstrap(ch)) =
									self.pending_kad_queries.remove(&id.into())
								{
									_ = ch.send(Ok(()));
								}
							},
							Err(err) => {
								debug!("Bootstrap error event. Error: {:?}.", err);
								if let Some(QueryChannel::Bootstrap(ch)) =
									self.pending_kad_queries.remove(&id.into())
								{
									_ = ch.send(Err(err.into()));
								}
							},
						},
						_ => {},
					},
				}
			},
			SwarmEvent::Behaviour(BehaviourEvent::Identify(event)) => {
				// record Indetify Behaviour events
				self.metrics.record(&event);

				match event {
					IdentifyEvent::Received {
						peer_id,
						info: Info {
							listen_addrs,
							protocols,
							..
						},
					} => {
						debug!(
							"Identify Received event. PeerId: {:?}. Listen address: {:?}",
							peer_id, listen_addrs
						);

						if protocols
							.iter()
							.any(|p| p.as_bytes() == protocol::DEFAULT_PROTO_NAME)
						{
							for addr in listen_addrs {
								self.swarm
									.behaviour_mut()
									.kademlia
									.add_address(&peer_id, addr);
							}
						}
					},
					IdentifyEvent::Sent { peer_id } => {
						debug!("Identify Sent event. PeerId: {:?}", peer_id);
					},
					IdentifyEvent::Pushed { peer_id } => {
						debug!("Identify Pushed event. PeerId: {:?}", peer_id);
					},
					IdentifyEvent::Error { peer_id, error } => {
						debug!(
							"Identify Error event. PeerId: {:?}. Error: {:?}",
							peer_id, error
						);
					},
				}
			},
			SwarmEvent::Behaviour(BehaviourEvent::Mdns(event)) => match event {
				MdnsEvent::Discovered(addrs_list) => {
					for (peer_id, multiaddr) in addrs_list {
						debug!(
							"MDNS got peer with ID: {:#?} and Address: {:#?}",
							peer_id, multiaddr
						);
						self.swarm
							.behaviour_mut()
							.kademlia
							.add_address(&peer_id, multiaddr);
					}
				},
				_ => (),
			},
			swarm_event => {
				// record Swarm events
				self.metrics.record(&swarm_event);

				match swarm_event {
					SwarmEvent::NewListenAddr { address, .. } => {
						let local_peer_id = *self.swarm.local_peer_id();
						info!(
							"Local node is listening on {:?}",
							address.with(Protocol::P2p(local_peer_id.into()))
						);
					},
					SwarmEvent::ConnectionClosed {
						peer_id,
						endpoint,
						num_established,
						cause,
					} => {
						trace!("Connection closed. PeerID: {:?}. Endpoint: {:?}. Num establ: {:?}. Cause: {:?}", peer_id, endpoint, num_established, cause);
					},
					SwarmEvent::IncomingConnection {
						local_addr,
						send_back_addr,
					} => {
						trace!(
							"Incoming connection from address: {:?}. Local address: {:?}",
							send_back_addr,
							local_addr
						);
					},
					SwarmEvent::IncomingConnectionError {
						local_addr,
						send_back_addr,
						error,
					} => {
						trace!(
							"Incoming connection error from address: {:?}. Local address: {:?}. Error: {:?}",
							send_back_addr, local_addr, error
						)
					},
					SwarmEvent::ConnectionEstablished {
						peer_id, endpoint, ..
					} => {
						trace!(
							"Connection established. PeerID: {:?}. Endpoint: {:?}.",
							peer_id,
							endpoint
						);
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
						trace!(
							"Outgoing connection error: {:?}. PeerId: {:?}",
							error,
							peer_id
						);
						if let Some(peer_id) = peer_id {
							if let Some(ch) = self.pending_dials.remove(&peer_id) {
								_ = ch.send(Err(error.into()));
							}
						}
					},
					SwarmEvent::Dialing(peer_id) => debug!("Dialing {}", peer_id),
					_ => {},
				}
			},
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
					// TODO: Implement logic for peer thats already beeing dialed
					debug!("Trying to redial peer: {:?}", peer_id);
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
	metrics: Metrics,
	port_reuse: bool,
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
	let local_peer_id = PeerId::from(id_keys.public());
	info!("Local peer id: {:?}", local_peer_id);

	// try to get psk
	let psk: Option<PreSharedKey> = get_psk(psk_path)?
		.map(|text| PreSharedKey::from_str(&text))
		.transpose()?;
	// create transport
	let transport = setup_transport(&id_keys, psk, port_reuse);

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
			identify: IdentifyBehaviour::new(IdentifyConfig::new(
				"/avail_kad/id/1.0.0".to_string(),
				id_keys.public(),
			)),
			mdns: TokioMdns::new(MdnsConfig::default())?,
			ping: PingBehaviour::new(PingConfig::new()),
			keep_alive: KeepAliveBehaviour::default(),
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
		EventLoop::new(swarm, command_receiver, event_sender, metrics),
	))
}

/// Read the pre-shared key file from the given directory
fn get_psk(location: &String) -> Result<Option<String>> {
	let path = Path::new(location);
	match fs::read_to_string(path) {
		Ok(text) => Ok(Some(text)),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
		Err(e) => Err(e.into()),
	}
}

fn setup_transport(
	key_pair: &Keypair,
	psk: Option<PreSharedKey>,
	port_reuse: bool,
) -> transport::Boxed<(PeerId, StreamMuxerBox)> {
	let noise = NoiseAuthenticated::xx(&key_pair).unwrap();

	let base_transport =
		TokioTcpTransport::new(GenTcpConfig::default().nodelay(true).port_reuse(port_reuse));
	let maybe_encrypted = match psk {
		Some(psk) => EitherTransport::Left(
			base_transport.and_then(move |socket, _| PnetConfig::new(psk).handshake(socket)),
		),
		None => EitherTransport::Right(base_transport),
	};

	maybe_encrypted
		.upgrade(Version::V1)
		.authenticate(noise)
		.multiplex(YamuxConfig::default())
		.timeout(Duration::from_secs(20))
		.boxed()
}
