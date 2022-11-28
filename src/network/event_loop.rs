use std::{
	collections::{hash_map, HashMap},
	sync::Arc,
};

use anyhow::Result;
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use super::client::Command;
use super::stream::{Event, NetworkEvents};

use libp2p::{
	core::either::EitherError,
	identify::{self, Event as IdentifyEvent, Info},
	kad::{
		protocol, store::MemoryStore, BootstrapOk, GetRecordOk, InboundRequest, Kademlia,
		KademliaConfig, KademliaEvent, PeerRecord, PutRecordOk, QueryId, QueryResult,
	},
	mdns::{MdnsConfig, MdnsEvent, TokioMdns},
	metrics::{Metrics, Recorder},
	multiaddr::Protocol,
	swarm::SwarmEvent,
	NetworkBehaviour as LibP2PBehaviour, PeerId, Swarm,
};
use tracing::{debug, info, trace};

#[derive(Debug)]
enum QueryChannel {
	GetRecord(oneshot::Sender<Result<Vec<PeerRecord>>>),
	PutRecord(oneshot::Sender<Result<()>>),
	Bootstrap(oneshot::Sender<Result<()>>),
}

#[derive(LibP2PBehaviour)]
#[behaviour(out_event = "BehaviourEvent")]
pub struct NetworkBehaviour {
	kademlia: Kademlia<MemoryStore>,
	identify: identify::Behaviour,
	mdns: TokioMdns,
}

impl NetworkBehaviour {
	pub fn new(
		id: PeerId,
		kad_store: MemoryStore,
		kad_cfg: KademliaConfig,
		identify_cfg: identify::Config,
	) -> Result<Self> {
		let mdns = TokioMdns::new(MdnsConfig::default())?;
		Ok(Self {
			kademlia: Kademlia::with_config(id, kad_store, kad_cfg),
			identify: identify::Behaviour::new(identify_cfg),
			mdns,
		})
	}
}

#[derive(Debug)]
pub enum BehaviourEvent {
	Kademlia(KademliaEvent),
	Identify(IdentifyEvent),
	Mdns(MdnsEvent),
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

pub struct EventLoop {
	swarm: Swarm<NetworkBehaviour>,
	command_receiver: mpsc::Receiver<Command>,
	network_events: Arc<NetworkEvents>,
	pending_dials: HashMap<PeerId, oneshot::Sender<Result<(), anyhow::Error>>>,
	pending_kad_queries: HashMap<QueryId, QueryChannel>,
	pending_kad_routing: HashMap<PeerId, oneshot::Sender<Result<()>>>,
	metrics: Metrics,
}

impl EventLoop {
	pub fn new(
		swarm: Swarm<NetworkBehaviour>,
		command_receiver: mpsc::Receiver<Command>,
		metrics: Metrics,
		network_events: Arc<NetworkEvents>,
	) -> Self {
		Self {
			swarm,
			command_receiver,
			network_events,
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
			EitherError<EitherError<std::io::Error, std::io::Error>, void::Void>,
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
						// this event is of a particular interest for our first node in the network
						self.network_events
							.notify(Event::ConnectionEstablished { peer_id, endpoint })
							.await;
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
