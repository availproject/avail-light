use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use futures::StreamExt;
use libp2p::autonat::Config as AutoNatConfig;
use libp2p::swarm::ConnectionHandlerUpgrErr;
use tokio::sync::{mpsc, oneshot};

use super::client::Command;
use super::stream::{Event, NetworkEvents};

use libp2p::{
	autonat::{Behaviour as AutonatBehaviour, Event as AutonatEvent},
	core::either::EitherError,
	identify::{
		Behaviour as IdentifyBehaviour, Config as IdentifyConfig, Event as IdentifyEvent, Info,
	},
	kad::{
		protocol, store::MemoryStore, BootstrapOk, GetRecordOk, InboundRequest, Kademlia,
		KademliaConfig, KademliaEvent, PeerRecord, PutRecordOk, QueryId, QueryResult,
	},
	mdns::{MdnsConfig, MdnsEvent, TokioMdns},
	metrics::{Metrics, Recorder},
	multiaddr::Protocol,
	ping::{self, Behaviour as PingBehaviour, Config as PingConfig, Event as PingEvent},
	swarm::{keep_alive::Behaviour as KeepAliveBehaviour, ConnectionError, SwarmEvent},
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
	identify: IdentifyBehaviour,
	mdns: TokioMdns,
	ping: PingBehaviour,
	keep_alive: KeepAliveBehaviour,
	auto_nat: AutonatBehaviour,
}

impl NetworkBehaviour {
	pub fn new(
		local_peer_id: PeerId,
		kad_store: MemoryStore,
		kad_cfg: KademliaConfig,
		identify_cfg: IdentifyConfig,
		autonat_cfg: AutoNatConfig,
	) -> Result<Self> {
		let mdns = TokioMdns::new(MdnsConfig::default())?;
		Ok(Self {
			kademlia: Kademlia::with_config(local_peer_id, kad_store, kad_cfg),
			identify: IdentifyBehaviour::new(identify_cfg),
			mdns,
			ping: PingBehaviour::new(PingConfig::new()),
			keep_alive: KeepAliveBehaviour::default(),
			auto_nat: AutonatBehaviour::new(local_peer_id, autonat_cfg),
		})
	}
}

#[derive(Debug)]
pub enum BehaviourEvent {
	Kademlia(KademliaEvent),
	Identify(IdentifyEvent),
	Mdns(MdnsEvent),
	Ping(PingEvent),
	Autonat(AutonatEvent),
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

impl From<AutonatEvent> for BehaviourEvent {
	fn from(event: AutonatEvent) -> Self {
		BehaviourEvent::Autonat(event)
	}
}

pub struct EventLoop {
	swarm: Swarm<NetworkBehaviour>,
	command_receiver: mpsc::Receiver<Command>,
	network_events: Arc<NetworkEvents>,
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
					EitherError<
						EitherError<EitherError<std::io::Error, std::io::Error>, void::Void>,
						ping::Failure,
					>,
					void::Void,
				>,
				ConnectionHandlerUpgrErr<std::io::Error>,
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
						debug!("Routing updated. Peer: {peer:?}. is_new_peer: {is_new_peer:?}. Addresses: {addresses:#?}. Old peer: {old_peer:#?}");
						if let Some(ch) = self.pending_kad_routing.remove(&peer.into()) {
							_ = ch.send(Ok(()));
						}
					},
					KademliaEvent::RoutablePeer { peer, address } => {
						debug!("RoutablePeer. Peer: {peer:?}.  Address: {address:?}");
					},
					KademliaEvent::UnroutablePeer { peer } => {
						debug!("UnroutablePeer. Peer: {peer:?}");
					},
					KademliaEvent::PendingRoutablePeer { peer, address } => {
						debug!("Pending routablePeer. Peer: {peer:?}.  Address: {address:?}");
					},
					KademliaEvent::InboundRequest { request } => {
						trace!("Inbound request: {:?}", request);
						if let InboundRequest::PutRecord { source, record, .. } = request {
							if let Some(block_ref) = record {
								trace!(
									"Inbound PUT request record key: {:?}. Source: {source:?}",
									block_ref.key,
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
								trace!("BootstrapOK event. PeerID: {peer:?}. Num remaining: {num_remaining:?}.");
								if num_remaining == 0 {
									if let Some(QueryChannel::Bootstrap(ch)) =
										self.pending_kad_queries.remove(&id.into())
									{
										_ = ch.send(Ok(()));
									}
								}
							},
							Err(err) => {
								trace!("Bootstrap error event. Error: {err:?}.");
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
						debug!("Identify Received event. PeerId: {peer_id:?}. Listen address: {listen_addrs:?}");

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
						debug!("Identify Sent event. PeerId: {peer_id:?}");
					},
					IdentifyEvent::Pushed { peer_id } => {
						debug!("Identify Pushed event. PeerId: {peer_id:?}");
					},
					IdentifyEvent::Error { peer_id, error } => {
						debug!("Identify Error event. PeerId: {peer_id:?}. Error: {error:?}");
					},
				}
			},
			SwarmEvent::Behaviour(BehaviourEvent::Mdns(event)) => match event {
				MdnsEvent::Discovered(addrs_list) => {
					for (peer_id, multiaddr) in addrs_list {
						debug!("MDNS got peer with ID: {peer_id:#?} and Address: {multiaddr:#?}");
						self.swarm
							.behaviour_mut()
							.kademlia
							.add_address(&peer_id, multiaddr);
					}
				},
				MdnsEvent::Expired(addrs_list) => {
					for (peer_id, multiaddr) in addrs_list {
						debug!("MDNS got expired peer with ID: {peer_id:#?} and Address: {multiaddr:#?}");

						if !self.swarm.behaviour_mut().mdns.has_node(&peer_id) {
							self.swarm
								.behaviour_mut()
								.kademlia
								.remove_address(&peer_id, &multiaddr);
						}
					}
				},
			},
			SwarmEvent::Behaviour(BehaviourEvent::Autonat(event)) => match event {
				AutonatEvent::InboundProbe(e) => {
					debug!("AutoNat Inbound Probe: {:#?}", e);
				},
				AutonatEvent::OutboundProbe(e) => {
					debug!("AutoNat Outbound Probe: {:#?}", e);
				},
				AutonatEvent::StatusChanged { old, new } => {
					debug!(
						"AutoNat Old status: {:#?}. AutoNat New status: {:#?}",
						old, new
					);
				},
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
						trace!("Connection closed. PeerID: {peer_id:?}. Address: {:?}. Num establ: {num_established:?}. Cause: {cause:?}", endpoint.get_remote_address());

						if let Some(cause) = cause {
							match cause {
								// remove peer with failed connection
								ConnectionError::IO(_) | ConnectionError::Handler(_) => {
									self.swarm.behaviour_mut().kademlia.remove_peer(&peer_id);
								},
								// ignore Keep alive timeout error
								// and allow redials for this type of error
								_ => {},
							}
						}
					},
					SwarmEvent::IncomingConnection {
						local_addr,
						send_back_addr,
					} => {
						trace!("Incoming connection from address: {send_back_addr:?}. Local address: {local_addr:?}");
					},
					SwarmEvent::IncomingConnectionError {
						local_addr,
						send_back_addr,
						error,
					} => {
						trace!("Incoming connection error from address: {send_back_addr:?}. Local address: {local_addr:?}. Error: {error:?}.")
					},
					SwarmEvent::ConnectionEstablished {
						peer_id, endpoint, ..
					} => {
						trace!(
							"Connection established. PeerID: {peer_id:?}. Endpoint: {endpoint:?}."
						);

						// this event is of a particular interest for our first node in the network
						self.network_events
							.notify(Event::ConnectionEstablished { peer_id, endpoint })
							.await;
					},
					SwarmEvent::OutgoingConnectionError { peer_id, error } => {
						trace!("Outgoing connection error: {error:?}. PeerId: {peer_id:?}");
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
