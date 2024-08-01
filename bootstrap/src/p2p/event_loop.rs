use anyhow::{anyhow, Result};
use libp2p::{
	autonat::{self, InboundProbeEvent, OutboundProbeEvent},
	futures::StreamExt,
	identify::{Event as IdentifyEvent, Info},
	kad::{self, BootstrapOk, QueryId, QueryResult},
	multiaddr::Protocol,
	swarm::SwarmEvent,
	Multiaddr, PeerId, Swarm,
};
use std::{
	collections::{hash_map, HashMap},
	str::FromStr,
	time::Duration,
};
use tokio::{
	sync::{mpsc, oneshot},
	time::{interval_at, Instant, Interval},
};
use tracing::{debug, trace};

use crate::types::AgentVersion;

use super::{client::Command, Behaviour, BehaviourEvent};

enum QueryChannel {
	Bootstrap(oneshot::Sender<Result<()>>),
}

enum SwarmChannel {
	Dial(oneshot::Sender<Result<()>>),
	ConnectionEstablished(oneshot::Sender<(PeerId, Multiaddr)>),
}

// BootstrapState keeps track of all things bootstrap related
struct BootstrapState {
	// referring to this initial bootstrap process,
	// one that runs when this node starts up
	is_startup_done: bool,
	// timer that is responsible for firing periodic bootstraps
	timer: Interval,
}

pub struct EventLoop {
	swarm: Swarm<Behaviour>,
	command_receiver: mpsc::Receiver<Command>,
	pending_kad_queries: HashMap<QueryId, QueryChannel>,
	pending_kad_routing: HashMap<PeerId, oneshot::Sender<Result<()>>>,
	pending_swarm_events: HashMap<PeerId, SwarmChannel>,
	bootstrap: BootstrapState,
}

impl EventLoop {
	pub fn new(
		swarm: Swarm<Behaviour>,
		command_receiver: mpsc::Receiver<Command>,
		bootstrap_interval: Duration,
	) -> Self {
		Self {
			swarm,
			command_receiver,
			pending_kad_queries: Default::default(),
			pending_kad_routing: Default::default(),
			pending_swarm_events: Default::default(),
			bootstrap: BootstrapState {
				is_startup_done: false,
				timer: interval_at(Instant::now() + bootstrap_interval, bootstrap_interval),
			},
		}
	}

	pub async fn run(mut self) {
		loop {
			tokio::select! {
				event = self.swarm.next() => self.handle_event(event.expect("Swarm stream should be infinite")).await,
				command = self.command_receiver.recv() => match command {
					Some(cmd) => self.handle_command(cmd).await,
					// command channel closed,
					// shutting down whole network event loop
					None => return,
				},
				_ = self.bootstrap.timer.tick() => self.handle_periodic_bootstraps(),
			}
		}
	}
	#[tracing::instrument(level = "trace", skip(self))]
	async fn handle_event(&mut self, event: SwarmEvent<BehaviourEvent>) {
		match event {
			SwarmEvent::Behaviour(BehaviourEvent::Kademlia(kad_event)) => match kad_event {
				kad::Event::RoutingUpdated {
					peer,
					is_new_peer,
					addresses,
					old_peer,
					..
				} => {
					trace!("Routing updated. Peer: {peer:?}. Is new Peer: {is_new_peer:?}. Addresses: {addresses:#?}. Old Peer: {old_peer:#?}");
					if let Some(ch) = self.pending_kad_routing.remove(&peer) {
						_ = ch.send(Ok(()));
					}
				},
				kad::Event::OutboundQueryProgressed {
					id,
					result: QueryResult::Bootstrap(bootstrap_result),
					..
				} => {
					match bootstrap_result {
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
									// we can say that the initial bootstrap at initialization is done
									self.bootstrap.is_startup_done = true;
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
					}
				},
				_ => {},
			},
			SwarmEvent::Behaviour(BehaviourEvent::Identify(IdentifyEvent::Received {
				peer_id,
				info:
					Info {
						listen_addrs,
						agent_version,
						protocol_version,
						protocols,
						..
					},
			})) => {
				trace!("Identity Received from: {peer_id:?} on listen address: {listen_addrs:?}.");
				let incoming_peer_agent_version = match AgentVersion::from_str(&agent_version) {
					Ok(agent) => agent,
					Err(e) => {
						debug!("Error parsing incoming agent version: {e}");
						return;
					},
				};
				trace![
					"Identify agent version: {}. Identify protocol version: {}.",
					incoming_peer_agent_version,
					protocol_version
				];
				if !incoming_peer_agent_version.is_supported() {
					debug!(
						"Unsupported release version: {}",
						incoming_peer_agent_version.release_version
					);
					self.swarm.behaviour_mut().kademlia.remove_peer(&peer_id);
					return;
				}

				if protocols.contains(&self.swarm.behaviour_mut().kademlia.protocol_names()[0]) {
					trace!("Adding peer {peer_id} to routing table.");
					for addr in listen_addrs {
						self.swarm
							.behaviour_mut()
							.kademlia
							.add_address(&peer_id, addr);
					}
				} else {
					// Block and remove non-Avail peers
					debug!("Removing and blocking non-avail peer from routing table. Peer: {peer_id}. Agent: {agent_version}. Protocol: {protocol_version}");
					self.swarm.behaviour_mut().kademlia.remove_peer(&peer_id);
				}
			},
			SwarmEvent::Behaviour(BehaviourEvent::AutoNat(autonat_event)) => match autonat_event {
				autonat::Event::InboundProbe(inbound_event) => match inbound_event {
					InboundProbeEvent::Error { peer, error, .. } => {
						debug!(
							"AutoNAT Inbound Probe failed with Peer: {:?}. Error: {:?}.",
							peer, error
						);
					},
					_ => {
						trace!("AutoNAT Inbound Probe: {:#?}", inbound_event);
					},
				},
				autonat::Event::OutboundProbe(outbound_event) => match outbound_event {
					OutboundProbeEvent::Error { peer, error, .. } => {
						debug!(
							"AutoNAT Outbound Probe failed with Peer: {:#?}. Error: {:?}",
							peer, error
						);
					},
					_ => {
						trace!("AutoNAT Outbound Probe: {:#?}", outbound_event);
					},
				},

				autonat::Event::StatusChanged { old, new } => {
					debug!(
						"AutoNAT Old status: {:#?}. AutoNAT New status: {:#?}",
						old, new
					);
				},
			},
			SwarmEvent::ConnectionClosed {
				peer_id,
				endpoint,
				num_established,
				cause,
				..
			} => {
				trace!("Connection closed. PeerID: {peer_id:?}. Address: {:?}. Num established: {num_established:?}. Cause: {cause:?}.", endpoint.get_remote_address());
			},

			SwarmEvent::OutgoingConnectionError {
				connection_id,
				peer_id: Some(peer_id),
				error,
			} => {
				trace!("Outgoing connection error. Connection id: {connection_id}. Peer: {peer_id}. Error: {error}.");

				if let Some(SwarmChannel::Dial(ch)) = self.pending_swarm_events.remove(&peer_id) {
					_ = ch.send(Err(anyhow!(error)));
				}
			},
			SwarmEvent::ConnectionEstablished {
				endpoint, peer_id, ..
			} => {
				// while waiting for a first successful connection,
				// we're interested in a case where we are dialing back
				if endpoint.is_dialer() {
					if let Some(event) = self.pending_swarm_events.remove(&peer_id) {
						match event {
							// check if there is a command waiting for a response for established 1st connection
							SwarmChannel::ConnectionEstablished(ch) => {
								// signal back that we have successfully established a connection,
								// give us back PeerId and Multiaddress
								let addr = endpoint.get_remote_address().to_owned();
								_ = ch.send((peer_id, addr));
							},
							SwarmChannel::Dial(ch) => {
								// signal back that dial was a success
								_ = ch.send(Ok(()));
							},
						}
					}
				}
			},
			SwarmEvent::NewListenAddr { address, .. } => {
				let local_peer_id = *self.swarm.local_peer_id();
				debug!(
					"Local node is listening on: {:?}",
					address.with(Protocol::P2p(local_peer_id))
				)
			},
			_ => {},
		}
	}

	async fn handle_command(&mut self, command: Command) {
		match command {
			Command::StartListening {
				addr,
				response_sender,
			} => {
				_ = match self.swarm.listen_on(addr) {
					Ok(_) => response_sender.send(Ok(())),
					Err(err) => response_sender.send(Err(err.into())),
				}
			},
			Command::DialPeer {
				peer_id,
				peer_address,
				response_sender,
			} => {
				if let hash_map::Entry::Vacant(e) = self.pending_swarm_events.entry(peer_id) {
					match self.swarm.dial(peer_address.with(Protocol::P2p(peer_id))) {
						Ok(()) => {
							e.insert(SwarmChannel::Dial(response_sender));
						},
						Err(e) => {
							let _ = response_sender.send(Err(anyhow!(e)));
						},
					}
				} else {
					todo!("Already dialing peer.");
				}
			},
			Command::AddAddress {
				peer_id,
				multiaddr,
				response_sender,
			} => {
				self.swarm
					.behaviour_mut()
					.kademlia
					.add_address(&peer_id, multiaddr);
				self.pending_kad_routing.insert(peer_id, response_sender);
			},
			Command::Bootstrap { response_sender } => {
				match self.swarm.behaviour_mut().kademlia.bootstrap() {
					Ok(query_id) => {
						self.pending_kad_queries
							.insert(query_id, QueryChannel::Bootstrap(response_sender));
					},
					// no available peers for bootstrap
					// send error immediately through response channel
					Err(err) => {
						_ = response_sender.send(Err(err.into()));
					},
				}
			},
			Command::WaitConnection {
				peer_id,
				response_sender,
			} => match peer_id {
				// this means that we're waiting on a connection from
				// a peer with provided PeerId
				Some(id) => {
					self.pending_swarm_events
						.insert(id, SwarmChannel::ConnectionEstablished(response_sender));
				},
				// sending no particular PeerId means that we're
				// waiting someone to establish connection with us
				None => {
					self.pending_swarm_events.insert(
						self.swarm.local_peer_id().to_owned(),
						SwarmChannel::ConnectionEstablished(response_sender),
					);
				},
			},
			Command::CountDHTPeers { response_sender } => {
				let mut total_peers: usize = 0;
				for bucket in self.swarm.behaviour_mut().kademlia.kbuckets() {
					total_peers += bucket.num_entries();
				}
				_ = response_sender.send(total_peers);
			},
			Command::GetMultiaddress { response_sender } => {
				let last_address = self.swarm.external_addresses().last();
				_ = response_sender.send(last_address.cloned());
			},
		}
	}

	fn handle_periodic_bootstraps(&mut self) {
		// periodic bootstraps should only start after the initial one is done
		if self.bootstrap.is_startup_done {
			debug!("Starting periodic Bootstrap.");
			_ = self.swarm.behaviour_mut().kademlia.bootstrap();
		}
	}
}
