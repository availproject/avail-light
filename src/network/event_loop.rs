use std::collections::HashMap;

use anyhow::Result;
use async_std::stream::StreamExt;
use itertools::Either;
use rand::seq::SliceRandom;
use tokio::sync::{mpsc, oneshot};
use void::Void;

use super::{
	client::{Command, NumSuccPut},
	Behaviour, BehaviourEvent, Event,
};

use libp2p::{
	autonat::{Event as AutonatEvent, NatStatus},
	dcutr::{
		inbound::UpgradeError as InboundUpgradeError,
		outbound::UpgradeError as OutboundUpgradeError, Event as DcutrEvent,
	},
	identify::{Event as IdentifyEvent, Info},
	kad::{
		BootstrapOk, GetRecordOk, InboundRequest, KademliaEvent, PeerRecord, QueryId, QueryResult,
		Record,
	},
	mdns::Event as MdnsEvent,
	metrics::{Metrics, Recorder},
	multiaddr::Protocol,
	ping,
	relay::{
		inbound::hop::FatalUpgradeError as InboundHopFatalUpgradeError,
		inbound::stop::FatalUpgradeError as InboundStopFatalUpgradeError,
		outbound::hop::FatalUpgradeError as OutboundHopFatalUpgradeError,
		outbound::stop::FatalUpgradeError as OutboundStopFatalUpgradeError, Event as RelayEvent,
	},
	swarm::{
		dial_opts::{DialOpts, PeerCondition},
		ConnectionError, ConnectionHandlerUpgrErr, SwarmEvent,
	},
	Multiaddr, PeerId, Swarm,
};
use tracing::{debug, error, info, trace};

#[derive(Debug)]
enum QueryChannel {
	GetRecord(oneshot::Sender<Result<PeerRecord>>),
	PutRecordBatch(oneshot::Sender<NumSuccPut>),
	Bootstrap(oneshot::Sender<Result<()>>),
}

struct RelayReservation {
	id: PeerId,
	address: Multiaddr,
	is_reserved: bool,
}

pub struct EventLoop {
	swarm: Swarm<Behaviour>,
	command_receiver: mpsc::Receiver<Command>,
	output_senders: Vec<mpsc::Sender<Event>>,
	pending_kad_queries: HashMap<QueryId, QueryChannel>,
	pending_kad_routing: HashMap<PeerId, oneshot::Sender<Result<()>>>,
	pending_kad_query_batch: HashMap<QueryId, Option<Result<()>>>,
	pending_batch_complete: Option<QueryChannel>,
	metrics: Metrics,
	relay_nodes: Vec<(PeerId, Multiaddr)>,
	relay_reservation: RelayReservation,
}

type FatalInHopOrOutStop = Either<InboundHopFatalUpgradeError, OutboundStopFatalUpgradeError>;

type FatalInStopOrOutHop = Either<InboundStopFatalUpgradeError, OutboundHopFatalUpgradeError>;

type Fatal = Either<
	Either<Either<ConnectionHandlerUpgrErr<FatalInHopOrOutStop>, Void>, std::io::Error>,
	Either<ConnectionHandlerUpgrErr<FatalInStopOrOutHop>, Void>,
>;

type Upgrade = Either<
	ConnectionHandlerUpgrErr<Either<InboundUpgradeError, OutboundUpgradeError>>,
	Either<ConnectionHandlerUpgrErr<std::io::Error>, Void>,
>;

type FatalOrUpgrade = Either<Fatal, Upgrade>;

type UpgradeError = Either<
	Either<Either<FatalOrUpgrade, std::io::Error>, ConnectionHandlerUpgrErr<std::io::Error>>,
	Void,
>;

type UpgradeErrorOrPingFailure = Either<UpgradeError, ping::Failure>;

impl EventLoop {
	pub fn new(
		swarm: Swarm<Behaviour>,
		command_receiver: mpsc::Receiver<Command>,
		metrics: Metrics,
		relay_nodes: Vec<(PeerId, Multiaddr)>,
	) -> Self {
		Self {
			swarm,
			command_receiver,
			output_senders: Vec::new(),
			pending_kad_queries: Default::default(),
			pending_kad_routing: Default::default(),
			pending_kad_query_batch: Default::default(),
			pending_batch_complete: None,
			metrics,
			relay_nodes,
			relay_reservation: RelayReservation {
				id: PeerId::random(),
				address: Multiaddr::empty(),
				is_reserved: Default::default(),
			},
		}
	}

	pub async fn run(mut self) {
		loop {
			tokio::select! {
				event = self.swarm.next() => self.handle_event(event.expect("Swarm stream should be infinite")).await,
				Some(command) = self.command_receiver.recv() => self.handle_command(command).await,
			}
		}
	}

	// Notify function is used to send network events to all listeners
	// through send channels that are able to send, otherwise channel is discarded
	fn notify(&mut self, event: Event) {
		self.output_senders
			.retain(|tx| tx.try_send(event.clone()).is_ok());
	}

	async fn handle_event(&mut self, event: SwarmEvent<BehaviourEvent, UpgradeErrorOrPingFailure>) {
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
						if let Some(ch) = self.pending_kad_routing.remove(&peer) {
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
						if let InboundRequest::PutRecord {
							source,
							record: Some(Record { key, .. }),
							..
						} = request
						{
							trace!("Inbound PUT request record key: {key:?}. Source: {source:?}",);
						}
					},
					KademliaEvent::OutboundQueryProgressed { id, result, .. } => match result {
						QueryResult::GetRecord(result) => match result {
							Ok(GetRecordOk::FoundRecord(record)) => {
								if let Some(QueryChannel::GetRecord(ch)) =
									self.pending_kad_queries.remove(&id)
								{
									_ = ch.send(Ok(record));
								}
							},
							Err(err) => {
								if let Some(QueryChannel::GetRecord(ch)) =
									self.pending_kad_queries.remove(&id)
								{
									_ = ch.send(Err(err.into()));
								}
							},
							_ => (),
						},
						QueryResult::PutRecord(result) => {
							if let Some(v) = self.pending_kad_query_batch.get_mut(&id) {
								match result {
									Ok(_) => *v = Some(Ok(())),
									Err(err) => *v = Some(Err(err.into())),
								};

								let cnt = self
									.pending_kad_query_batch
									.iter()
									.filter(|(_, elem)| elem.is_none())
									.count();

								if cnt == 0 {
									if let Some(QueryChannel::PutRecordBatch(ch)) =
										self.pending_batch_complete.take()
									{
										let count_success = self
											.pending_kad_query_batch
											.iter()
											.filter(|(_, elem)| {
												elem.is_some() && elem.as_ref().unwrap().is_ok()
											})
											.count();
										_ = ch.send(NumSuccPut(count_success));
									}
								}
							}
						},
						QueryResult::Bootstrap(result) => match result {
							Ok(BootstrapOk {
								peer,
								num_remaining,
							}) => {
								trace!("BootstrapOK event. PeerID: {peer:?}. Num remaining: {num_remaining:?}.");
								if num_remaining == 0 {
									if let Some(QueryChannel::Bootstrap(ch)) =
										self.pending_kad_queries.remove(&id)
									{
										_ = ch.send(Ok(()));
									}
								}
							},
							Err(err) => {
								trace!("Bootstrap error event. Error: {err:?}.");
								if let Some(QueryChannel::Bootstrap(ch)) =
									self.pending_kad_queries.remove(&id)
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
						info: Info { listen_addrs, .. },
					} => {
						debug!("Identity Received from: {peer_id:?} on listen address: {listen_addrs:?}");
						// before we try and do a reservation with the relay
						// we have to exchange observed addresses
						// in this case relay needs to tell us our own
						if peer_id == self.relay_reservation.id
							&& !self.relay_reservation.is_reserved
						{
							match self.swarm.listen_on(
								self.relay_reservation
									.address
									.clone()
									.with(Protocol::P2p(peer_id.into()))
									.with(Protocol::P2pCircuit),
							) {
								Ok(_) => {
									self.relay_reservation.is_reserved = true;
								},
								Err(e) => {
									error!("Local node failed to listen on relay address. Error: {e:#?}");
								},
							}
						}

						// only interested in addresses with actual Multiaddresses
						// ones that contains the 'p2p' tag
						let addrs = listen_addrs
							.into_iter()
							.filter(|a| a.to_string().contains(Protocol::P2p(peer_id.into()).tag()))
							.collect::<Vec<Multiaddr>>();

						for addr in addrs {
							self.swarm
								.behaviour_mut()
								.kademlia
								.add_address(&peer_id, addr.clone());

							// if address contains relay circuit tag,
							// dial that address for immediate Direct Connection Upgrade
							if *self.swarm.local_peer_id() != peer_id
								&& addr.to_string().contains(Protocol::P2pCircuit.tag())
							{
								_ = self.swarm.dial(
									DialOpts::peer_id(peer_id)
										.condition(PeerCondition::Disconnected)
										.addresses(vec![addr.with(Protocol::P2pCircuit)])
										.build(),
								);
							}
						}
					},
					IdentifyEvent::Sent { peer_id } => {
						debug!("Identity Sent event to: {peer_id:?}");
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
						if let Some(mdns) = self.swarm.behaviour_mut().mdns.as_ref() {
							if mdns.has_node(&peer_id) {
								self.swarm
									.behaviour_mut()
									.kademlia
									.remove_address(&peer_id, &multiaddr);
							}
						}
					}
				},
			},
			SwarmEvent::Behaviour(BehaviourEvent::AutoNat(event)) => match event {
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
					// check if went private or are private
					// if so, create reservation request with relay
					if new == NatStatus::Private || old == NatStatus::Private {
						info!("Autonat says we're still private.");
						// choose relay by random
						if let Some(relay) = self.relay_nodes.choose(&mut rand::thread_rng()) {
							let (relay_id, relay_addr) = relay.clone();
							// appoint this relay as our chosen one
							self.relay_reservation(relay_id, relay_addr);
						}
					};
				},
			},
			SwarmEvent::Behaviour(BehaviourEvent::Relay(event)) => match event {
				RelayEvent::ReservationReqAccepted { src_peer_id, .. } => {
					debug!("Relay accepted reservation request from: {src_peer_id:#?}");
				},
				RelayEvent::ReservationReqDenied { src_peer_id } => {
					debug!("Reservation request was denied for: {src_peer_id:#?}");
				},
				RelayEvent::ReservationTimedOut { src_peer_id } => {
					debug!("Reservation timed out for: {src_peer_id:#?}");
				},
				_ => {},
			},
			SwarmEvent::Behaviour(BehaviourEvent::RelayClient(event)) => {
				debug! {"Relay Client Event: {event:#?}"};
			},
			SwarmEvent::Behaviour(BehaviourEvent::Dcutr(event)) => match event {
				DcutrEvent::RemoteInitiatedDirectConnectionUpgrade {
					remote_peer_id,
					remote_relayed_addr,
				} => {
					debug!("Remote with ID: {remote_peer_id:#?} initiated Direct Connection Upgrade through address: {remote_relayed_addr:#?}");
				},
				DcutrEvent::InitiatedDirectConnectionUpgrade {
					remote_peer_id,
					local_relayed_addr,
				} => {
					debug!("Local node initiated Direct Connection Upgrade with remote: {remote_peer_id:#?} on address: {local_relayed_addr:#?}");
				},
				DcutrEvent::DirectConnectionUpgradeSucceeded { remote_peer_id } => {
					debug!("Hole punching succeeded with: {remote_peer_id:#?}")
				},
				DcutrEvent::DirectConnectionUpgradeFailed {
					remote_peer_id,
					error,
				} => {
					debug!("Hole punching failed with: {remote_peer_id:#?}. Error: {error:#?}");
				},
			},
			swarm_event => {
				// record Swarm events
				self.metrics.record(&swarm_event);

				match swarm_event {
					SwarmEvent::NewListenAddr { address, .. } => {
						info!("Local node is listening on {:?}", address);
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
						trace!("Connection established to: {peer_id:?} via: {endpoint:?}.");

						// this event is of a particular interest for our first node in the network
						self.notify(Event::ConnectionEstablished { peer_id, endpoint });
					},
					SwarmEvent::OutgoingConnectionError { peer_id, error } => {
						// remove error producing relay from pending dials
						trace!("Outgoing connection error: {error:?}");
						if let Some(peer_id) = peer_id {
							trace!("Error produced by peer with PeerId: {peer_id:?}");
							// if the peer giving us problems is the chosen relay
							// just remove it by reseting the reservatin state slot
							if self.relay_reservation.id == peer_id {
								self.reset_relay_reservation();
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
					.add_address(&peer_id, peer_addr);

				self.pending_kad_routing.insert(peer_id, sender);
			},
			Command::Stream { sender } => {
				self.output_senders.push(sender);
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
			Command::GetKadRecord { key, sender } => {
				let query_id = self.swarm.behaviour_mut().kademlia.get_record(key);

				self.pending_kad_queries
					.insert(query_id, QueryChannel::GetRecord(sender));
			},
			Command::PutKadRecordBatch {
				records,
				quorum,
				sender,
			} => {
				let mut ids: HashMap<QueryId, Option<Result<()>>> = Default::default();

				for record in records {
					let query_id = self
						.swarm
						.behaviour_mut()
						.kademlia
						.put_record(record, quorum)
						.expect("Unable to perform batch Kademlia PUT operation.");
					ids.insert(query_id, None);
				}
				self.pending_kad_query_batch = ids;
				self.pending_batch_complete = Some(QueryChannel::PutRecordBatch(sender));
			},
		}
	}

	fn reset_relay_reservation(&mut self) {
		self.relay_reservation = RelayReservation {
			id: PeerId::random(),
			address: Multiaddr::empty(),
			is_reserved: false,
		};
	}

	fn relay_reservation(&mut self, id: PeerId, address: Multiaddr) {
		self.relay_reservation = RelayReservation {
			id,
			address,
			is_reserved: false,
		};
	}
}
