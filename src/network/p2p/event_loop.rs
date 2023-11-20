use anyhow::{anyhow, Result};
use futures::StreamExt;
use libp2p::{
	autonat::{self, NatStatus},
	dcutr,
	identify::{self, Info},
	kad::{self, BootstrapOk, GetRecordOk, InboundRequest, QueryId, QueryResult},
	mdns,
	multiaddr::Protocol,
	swarm::{
		dial_opts::{DialOpts, PeerCondition},
		ConnectionError, SwarmEvent,
	},
	upnp, Multiaddr, PeerId, Swarm,
};
use rand::seq::SliceRandom;
use std::{collections::HashMap, time::Duration};
use tokio::{
	sync::oneshot,
	time::{interval_at, Instant, Interval},
};
use tracing::{debug, error, info, trace};

use super::{
	Behaviour, BehaviourEvent, CommandReceiver, DHTPutSuccess, EventLoopEntries, QueryChannel,
	SendableCommand,
};

// RelayState keeps track of all things relay related
struct RelayState {
	// id of the selected Relay that needs to be connected
	id: PeerId,
	// Multiaddress of the selected Relay that needs to be connected
	address: Multiaddr,
	// boolean value that signals if have established a circuit with the selected Relay
	is_circuit_established: bool,
	// list of available Relay nodes
	nodes: Vec<(PeerId, Multiaddr)>,
}

impl RelayState {
	fn reset(&mut self) {
		self.id = PeerId::random();
		self.address = Multiaddr::empty();
		self.is_circuit_established = false;
	}

	fn select_random(&mut self) {
		// choose relay by random
		if let Some(relay) = self.nodes.choose(&mut rand::thread_rng()) {
			let (id, addr) = relay.clone();
			// appoint this relay as our chosen one
			self.id = id;
			self.address = addr;
		}
	}
}

// BootstrapState keeps track of all things bootstrap related
struct BootstrapState {
	// referring to the initial bootstrap process,
	// one that runs when the Light Client node starts up
	is_startup_done: bool,
	// timer that is responsible for firing periodic bootstraps
	timer: Interval,
}

pub struct EventLoop {
	swarm: Swarm<Behaviour>,
	command_receiver: CommandReceiver,
	pending_kad_queries: HashMap<QueryId, QueryChannel>,
	pending_kad_routing: HashMap<PeerId, oneshot::Sender<Result<()>>>,
	pending_swarm_events: HashMap<PeerId, oneshot::Sender<Result<()>>>,
	relay: RelayState,
	bootstrap: BootstrapState,
	kad_remove_local_record: bool,
}

impl EventLoop {
	pub fn new(
		swarm: Swarm<Behaviour>,
		command_receiver: CommandReceiver,
		relay_nodes: Vec<(PeerId, Multiaddr)>,
		bootstrap_interval: Duration,
		kad_remove_local_record: bool,
	) -> Self {
		Self {
			swarm,
			command_receiver,
			pending_kad_queries: Default::default(),
			pending_kad_routing: Default::default(),
			pending_swarm_events: Default::default(),
			relay: RelayState {
				id: PeerId::random(),
				address: Multiaddr::empty(),
				is_circuit_established: false,
				nodes: relay_nodes,
			},
			bootstrap: BootstrapState {
				is_startup_done: false,
				timer: interval_at(Instant::now() + bootstrap_interval, bootstrap_interval),
			},
			kad_remove_local_record,
		}
	}

	pub async fn run(mut self) {
		loop {
			tokio::select! {
				event = self.swarm.next() => self.handle_event(event.expect("Swarm stream should be infinite")).await,
				command = self.command_receiver.recv() => match command {
					Some(c) => self.handle_command(c).await,
					// Command channel closed, thus shutting down the network event loop.
					None => return,
				},
				_ = self.bootstrap.timer.tick() => self.handle_periodic_bootstraps(),
			}
		}
	}

	#[tracing::instrument(level = "trace", skip(self))]
	async fn handle_event(&mut self, event: SwarmEvent<BehaviourEvent>) {
		match event {
			SwarmEvent::Behaviour(BehaviourEvent::Kademlia(event)) => {
				match event {
					kad::Event::RoutingUpdated {
						peer,
						is_new_peer,
						addresses,
						old_peer,
						..
					} => {
						trace!("Routing updated. Peer: {peer:?}. is_new_peer: {is_new_peer:?}. Addresses: {addresses:#?}. Old peer: {old_peer:#?}");
						if let Some(ch) = self.pending_kad_routing.remove(&peer) {
							_ = ch.send(Ok(()));
						}
					},
					kad::Event::RoutablePeer { peer, address } => {
						trace!("RoutablePeer. Peer: {peer:?}.  Address: {address:?}");
					},
					kad::Event::UnroutablePeer { peer } => {
						trace!("UnroutablePeer. Peer: {peer:?}");
					},
					kad::Event::PendingRoutablePeer { peer, address } => {
						trace!("Pending routablePeer. Peer: {peer:?}.  Address: {address:?}");
					},
					kad::Event::InboundRequest { request } => {
						trace!("Inbound request: {:?}", request);
						if let InboundRequest::PutRecord { source, record, .. } = request {
							match record {
								Some(rec) => {
									let key = &rec.key;
									trace!("Inbound PUT request record key: {key:?}. Source: {source:?}",);

									_ = self.swarm.behaviour_mut().kademlia.store_mut().put(rec);
								},
								None => {
									debug!("Received empty cell record from: {source:?}");
									return;
								},
							}
						}
					},
					kad::Event::OutboundQueryProgressed { id, result, .. } => match result {
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
							if let Some(QueryChannel::PutRecordBatch(success_tx)) =
								self.pending_kad_queries.remove(&id)
							{
								if let Ok(record) = result {
									// Remove local records for fat clients (memory optimization)
									debug!("Pruning local records on fat client");
									if self.kad_remove_local_record {
										self.swarm
											.behaviour_mut()
											.kademlia
											.remove_record(&record.key);
									}
									// signal back that this PUT request was a success,
									// so it can be accounted for
									_ = success_tx.send(DHTPutSuccess::Single).await;
								}
							}
						},
						QueryResult::Bootstrap(result) => match result {
							Ok(BootstrapOk {
								peer,
								num_remaining,
							}) => {
								debug!("BootstrapOK event. PeerID: {peer:?}. Num remaining: {num_remaining:?}.");
								if num_remaining == 0 {
									if let Some(QueryChannel::Bootstrap(ch)) =
										self.pending_kad_queries.remove(&id)
									{
										_ = ch.send(Ok(()));
										// we can say that the startup bootstrap is done here
										self.bootstrap.is_startup_done = true;
									}
								}
							},
							Err(err) => {
								debug!("Bootstrap error event. Error: {err:?}.");
								if let Some(QueryChannel::Bootstrap(ch)) =
									self.pending_kad_queries.remove(&id)
								{
									_ = ch.send(Err(err.into()));
								}
							},
						},
						_ => {},
					},
					_ => {},
				}
			},
			SwarmEvent::Behaviour(BehaviourEvent::Identify(event)) => {
				match event {
					identify::Event::Received {
						peer_id,
						info: Info { listen_addrs, .. },
					} => {
						debug!("Identity Received from: {peer_id:?} on listen address: {listen_addrs:?}");
						self.establish_relay_circuit(peer_id);

						// only interested in addresses with actual Multiaddresses
						// ones that contains the 'p2p' tag
						listen_addrs
							.into_iter()
							.filter(|a| a.to_string().contains(Protocol::P2p(peer_id).tag()))
							.for_each(|a| {
								self.swarm
									.behaviour_mut()
									.kademlia
									.add_address(&peer_id, a.clone());

								// if address contains relay circuit tag,
								// dial that address for immediate Direct Connection Upgrade
								if *self.swarm.local_peer_id() != peer_id
									&& a.to_string().contains(Protocol::P2pCircuit.tag())
								{
									_ = self.swarm.dial(
										DialOpts::peer_id(peer_id)
											.condition(PeerCondition::Disconnected)
											.addresses(vec![a.with(Protocol::P2pCircuit)])
											.build(),
									);
								}
							});
					},
					identify::Event::Sent { peer_id } => {
						trace!("Identity Sent event to: {peer_id:?}");
					},
					identify::Event::Pushed { peer_id, .. } => {
						trace!("Identify Pushed event. PeerId: {peer_id:?}");
					},
					identify::Event::Error { peer_id, error } => {
						trace!("Identify Error event. PeerId: {peer_id:?}. Error: {error:?}");
					},
				}
			},
			SwarmEvent::Behaviour(BehaviourEvent::Mdns(event)) => match event {
				mdns::Event::Discovered(addrs_list) => {
					for (peer_id, multiaddr) in addrs_list {
						trace!("MDNS got peer with ID: {peer_id:#?} and Address: {multiaddr:#?}");
						self.swarm
							.behaviour_mut()
							.kademlia
							.add_address(&peer_id, multiaddr);
					}
				},
				mdns::Event::Expired(addrs_list) => {
					for (peer_id, multiaddr) in addrs_list {
						trace!("MDNS got expired peer with ID: {peer_id:#?} and Address: {multiaddr:#?}");

						if self
							.swarm
							.behaviour_mut()
							.mdns
							.discovered_nodes()
							.any(|&p| p == peer_id)
						{
							self.swarm
								.behaviour_mut()
								.kademlia
								.remove_address(&peer_id, &multiaddr);
						}
					}
				},
			},
			SwarmEvent::Behaviour(BehaviourEvent::AutoNat(event)) => match event {
				autonat::Event::InboundProbe(e) => {
					trace!("AutoNat Inbound Probe: {:#?}", e);
				},
				autonat::Event::OutboundProbe(e) => {
					trace!("AutoNat Outbound Probe: {:#?}", e);
				},
				autonat::Event::StatusChanged { old, new } => {
					debug!(
						"AutoNat Old status: {:#?}. AutoNat New status: {:#?}",
						old, new
					);
					// check if went private or are private
					// if so, create reservation request with relay
					if new == NatStatus::Private || old == NatStatus::Private {
						info!("Autonat says we're still private.");
						// select a relay, try to dial it
						self.select_and_dial_relay();
					};
				},
			},
			SwarmEvent::Behaviour(BehaviourEvent::RelayClient(event)) => {
				trace! {"Relay Client Event: {event:#?}"};
			},
			SwarmEvent::Behaviour(BehaviourEvent::Dcutr(dcutr::Event {
				remote_peer_id,
				result,
			})) => match result {
				Ok(_) => trace!("Hole punching succeeded with: {remote_peer_id:#?}"),
				Err(err) => {
					trace!("Hole punching failed with: {remote_peer_id:#?}. Error: {err:#?}")
				},
			},
			SwarmEvent::Behaviour(BehaviourEvent::Upnp(event)) => match event {
				upnp::Event::NewExternalAddr(addr) => {
					info!("New external address: {addr}");
				},
				upnp::Event::GatewayNotFound => {
					trace!("Gateway does not support UPnP");
				},
				upnp::Event::NonRoutableGateway => {
					trace!("Gateway is not exposed directly to the public Internet, i.e. it itself has a private IP address.");
				},
				upnp::Event::ExpiredExternalAddr(addr) => {
					trace!("Gateway address expired: {addr}");
				},
			},
			swarm_event => {
				match swarm_event {
					SwarmEvent::NewListenAddr { address, .. } => {
						debug!("Local node is listening on {:?}", address);
					},
					SwarmEvent::ConnectionClosed {
						peer_id,
						endpoint,
						num_established,
						cause,
						..
					} => {
						trace!("Connection closed. PeerID: {peer_id:?}. Address: {:?}. Num established: {num_established:?}. Cause: {cause:?}", endpoint.get_remote_address());

						if let Some(ConnectionError::IO(_)) = cause {
							// remove peer with failed connection
							self.swarm.behaviour_mut().kademlia.remove_peer(&peer_id);
						}
					},
					SwarmEvent::IncomingConnection {
						local_addr,
						send_back_addr,
						..
					} => {
						trace!("Incoming connection from address: {send_back_addr:?}. Local address: {local_addr:?}");
					},
					SwarmEvent::IncomingConnectionError {
						local_addr,
						send_back_addr,
						error,
						..
					} => {
						trace!("Incoming connection error from address: {send_back_addr:?}. Local address: {local_addr:?}. Error: {error:?}.")
					},
					SwarmEvent::ConnectionEstablished {
						peer_id,
						endpoint,
						established_in,
						..
					} => {
						trace!("Connection established to: {peer_id:?} via: {endpoint:?} in {established_in:?}. ");
						// Notify the connections we're waiting on that we've connected successfully
						if let Some(ch) = self.pending_swarm_events.remove(&peer_id) {
							_ = ch.send(Ok(()));
						}
					},
					SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
						trace!("Outgoing connection error: {error:?}");

						if let Some(peer_id) = peer_id {
							trace!("OutgoingConnectionError by peer: {peer_id:?}. Error: {error}.");
							// Notify the connections we're waiting on an error has occured
							if let Some(ch) = self.pending_swarm_events.remove(&peer_id) {
								_ = ch.send(Err(error.into()));
							}
							// remove error producing relay from pending dials
							// if the peer giving us problems is the chosen relay
							// just remove it by resetting the reservation state slot
							if self.relay.id == peer_id {
								self.relay.reset();
							}
						}
					},
					SwarmEvent::Dialing {
						peer_id: Some(peer),
						connection_id,
					} => {
						trace!("Dialing: {}, on connection: {}", peer, connection_id);
					},
					_ => {},
				}
			},
		}
	}

	async fn handle_command(&mut self, mut command: SendableCommand) {
		if let Err(err) = command.run(EventLoopEntries::new(
			&mut self.swarm,
			&mut self.pending_kad_queries,
			&mut self.pending_kad_routing,
		)) {
			command.abort(anyhow!(err));
		}
	}

	fn handle_periodic_bootstraps(&mut self) {
		// commence with periodic bootstraps,
		// only when the initial startup bootstrap is done
		if self.bootstrap.is_startup_done {
			_ = self.swarm.behaviour_mut().kademlia.bootstrap();
		}
	}

	fn establish_relay_circuit(&mut self, peer_id: PeerId) {
		// before we try and create a circuit with the relay
		// we have to exchange observed addresses
		// in this case we're waiting on relay to tell us our own
		if peer_id == self.relay.id && !self.relay.is_circuit_established {
			match self
				.swarm
				.listen_on(self.relay.address.clone().with(Protocol::P2pCircuit))
			{
				Ok(_) => {
					info!("Relay circuit established with relay: {peer_id:?}");
					self.relay.is_circuit_established = true;
				},
				Err(e) => {
					// failed to establish a circuit, reset to try another relay
					self.relay.reset();
					error!("Local node failed to listen on relay address. Error: {e:#?}");
				},
			}
		}
	}

	fn select_and_dial_relay(&mut self) {
		// select a random relay from the list of known ones
		self.relay.select_random();

		// dial selected relay,
		// so we don't wait on swarm to do it eventually
		match self.swarm.dial(
			DialOpts::peer_id(self.relay.id)
				.condition(PeerCondition::NotDialing)
				.addresses(vec![self.relay.address.clone()])
				.build(),
		) {
			Ok(_) => {
				info!("Dialing Relay: {id:?} succeeded.", id = self.relay.id);
			},
			Err(e) => {
				// got an error while dialing,
				// better select a new relay and try again
				self.relay.reset();
				error!(
					"Dialing Relay: {id:?}, produced an error: {e:?}",
					id = self.relay.id
				);
			},
		}
	}
}
