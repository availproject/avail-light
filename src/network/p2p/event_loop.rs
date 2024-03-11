use color_eyre::{eyre::eyre, Result};
use futures::StreamExt;
use libp2p::{
	autonat::{self, NatStatus},
	dcutr,
	identify::{self, Info},
	identity::Keypair,
	kad::{
		self, BootstrapOk, GetRecordOk, InboundRequest, QueryId, QueryResult, QueryStats, RecordKey,
	},
	mdns,
	multiaddr::Protocol,
	ping,
	swarm::{
		dial_opts::{DialOpts, PeerCondition},
		ConnectionError, SwarmEvent,
	},
	upnp, Multiaddr, PeerId, Swarm,
};
use rand::seq::SliceRandom;
use std::{collections::HashMap, str::FromStr, sync::Arc, time::Duration};
use tokio::{
	sync::oneshot,
	time::{interval_at, Instant, Interval},
};
use tracing::{debug, error, info, trace, warn};

use crate::{
	network::p2p::kad_mem_store::MemoryStore,
	shutdown::Controller,
	telemetry::{MetricCounter, MetricValue, Metrics},
	types::{AgentVersion, IdentifyConfig, KademliaMode, LibP2PConfig, TimeToLive},
};

use super::{
	build_swarm, client::BlockStat, Behaviour, BehaviourEvent, CommandReceiver, EventLoopEntries,
	QueryChannel, SendableCommand,
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

struct EventLoopConfig {
	// Used for checking protocol version
	identity_data: IdentifyConfig,
	is_fat_client: bool,
	kad_record_ttl: TimeToLive,
}

pub struct EventLoop {
	swarm: Swarm<Behaviour>,
	// Tracking Kademlia events
	pending_kad_queries: HashMap<QueryId, QueryChannel>,
	// Tracking swarm events (i.e. peer dialing)
	pending_swarm_events: HashMap<PeerId, oneshot::Sender<Result<()>>>,
	relay: RelayState,
	bootstrap: BootstrapState,
	/// Blocks we monitor for PUT success rate
	active_blocks: HashMap<u32, BlockStat>,
	shutdown: Controller<String>,

	event_loop_config: EventLoopConfig,
}

#[derive(PartialEq, Debug)]
enum DHTKey {
	Cell(u32, u32, u32),
	Row(u32, u32),
}

impl TryFrom<RecordKey> for DHTKey {
	type Error = color_eyre::Report;

	fn try_from(key: RecordKey) -> std::result::Result<Self, Self::Error> {
		match *String::from_utf8(key.to_vec())?
			.split(':')
			.map(str::parse::<u32>)
			.collect::<std::result::Result<Vec<_>, _>>()?
			.as_slice()
		{
			[block_num, row_num] => Ok(DHTKey::Row(block_num, row_num)),
			[block_num, row_num, col_num] => Ok(DHTKey::Cell(block_num, row_num, col_num)),
			_ => Err(eyre!("Invalid DHT key")),
		}
	}
}

impl EventLoop {
	pub async fn new(
		cfg: LibP2PConfig,
		id_keys: &Keypair,
		is_fat_client: bool,
		is_ws_transport: bool,
		shutdown: Controller<String>,
	) -> Self {
		let bootstrap_interval = cfg.bootstrap_interval;
		let peer_id = id_keys.public().to_peer_id();
		let store = MemoryStore::with_config(peer_id, (&cfg).into());

		let swarm = build_swarm(&cfg, id_keys, store, is_ws_transport)
			.await
			.expect("Unable to build swarm.");

		Self {
			swarm,
			pending_kad_queries: Default::default(),
			pending_swarm_events: Default::default(),
			relay: RelayState {
				id: PeerId::random(),
				address: Multiaddr::empty(),
				is_circuit_established: false,
				nodes: cfg.relays,
			},
			bootstrap: BootstrapState {
				is_startup_done: false,
				timer: interval_at(Instant::now() + bootstrap_interval, bootstrap_interval),
			},
			active_blocks: Default::default(),
			shutdown,
			event_loop_config: EventLoopConfig {
				identity_data: cfg.identify,
				is_fat_client,
				kad_record_ttl: TimeToLive(cfg.kademlia.kad_record_ttl),
			},
		}
	}

	pub async fn run(mut self, metrics: Arc<impl Metrics>, mut command_receiver: CommandReceiver) {
		// shutdown will wait as long as this token is not dropped
		let _delay_token = self
			.shutdown
			.delay_token()
			.expect("There should not be any shutdowns at the begging of the P2P Event Loop");

		loop {
			tokio::select! {
				event = self.swarm.next() => self.handle_event(event.expect("Swarm stream should be infinite"), metrics.clone()).await,
				command = command_receiver.recv() => match command {
					Some(c) => self.handle_command(c).await,
					//
					None => {
						warn!("Command channel closed, exiting the network event loop");
						break;
					},
				},
				_ = self.bootstrap.timer.tick() => self.handle_periodic_bootstraps(),
				// if the shutdown was triggered,
				// break the loop immediately, proceed to the cleanup phase
				_ = self.shutdown.triggered_shutdown() => {
					info!("Shutdown triggered, exiting the network event loop");
					break;
				}
			}
		}
		self.disconnect_peers();
	}

	fn disconnect_peers(&mut self) {
		let connected_peers: Vec<PeerId> = self.swarm.connected_peers().cloned().collect();
		// close all active connections with other peers
		for peer in connected_peers {
			_ = self.swarm.disconnect_peer_id(peer);
		}
	}

	#[tracing::instrument(level = "trace", skip(self, metrics))]
	async fn handle_event(
		&mut self,
		event: SwarmEvent<BehaviourEvent>,
		metrics: Arc<impl Metrics>,
	) {
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
					kad::Event::InboundRequest { request } => match request {
						InboundRequest::GetRecord { .. } => {
							metrics.count(MetricCounter::IncomingGetRecord).await;
						},
						InboundRequest::PutRecord { source, record, .. } => {
							metrics.count(MetricCounter::IncomingPutRecord).await;
							match record {
								Some(mut record) => {
									let ttl = &self.event_loop_config.kad_record_ttl;

									// Set TTL for all incoming records
									// TTL will be set to a lower value between the local TTL and incoming record TTL
									record.expires = record.expires.min(ttl.expires());
									_ = self.swarm.behaviour_mut().kademlia.store_mut().put(record);
								},
								None => {
									debug!("Received empty cell record from: {source:?}");
									return;
								},
							}
						},
						_ => {},
					},
					kad::Event::ModeChanged { new_mode } => {
						trace!("Kademlia mode changed: {new_mode:?}");
						// TODO: dynamic change of identify protocols Kad mode
					},
					kad::Event::OutboundQueryProgressed {
						id, result, stats, ..
					} => match result {
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
						QueryResult::PutRecord(Err(error)) => {
							if self.pending_kad_queries.remove(&id).is_none() {
								return;
							};

							match error {
								kad::PutRecordError::QuorumFailed { key, .. } => {
									self.handle_put_result(key, stats, true, metrics).await;
								},
								kad::PutRecordError::Timeout { key, .. } => {
									self.handle_put_result(key, stats, true, metrics).await;
								},
							}
						},

						QueryResult::PutRecord(Ok(record)) => {
							if self.pending_kad_queries.remove(&id).is_none() {
								return;
							};
							self.handle_put_result(record.key.clone(), stats, false, metrics)
								.await;
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
				}
			},
			SwarmEvent::Behaviour(BehaviourEvent::Identify(event)) => match event {
				identify::Event::Received {
					peer_id,
					info:
						Info {
							listen_addrs,
							agent_version,
							protocol_version,
							..
						},
				} => {
					trace!(
						"Identity Received from: {peer_id:?} on listen address: {listen_addrs:?}"
					);
					let incoming_peer_agent_version = match AgentVersion::from_str(&agent_version) {
						Ok(agent) => agent,
						Err(e) => {
							debug!("Error parsing incoming agent version: {e}");
							return;
						},
					};
					if protocol_version == self.event_loop_config.identity_data.protocol_version {
						// Add peer to routing table only if it's in Kademlia server mode
						if incoming_peer_agent_version.kademlia_mode
							== KademliaMode::Server.to_string()
						{
							trace!("Adding peer {peer_id} to routing table.");
							for addr in listen_addrs {
								self.swarm
									.behaviour_mut()
									.kademlia
									.add_address(&peer_id, addr);
							}
						}
					} else {
						// Block and remove non-Avail peers
						debug!("Removing and blocking non-avail peer from routing table. Peer: {peer_id}. Agent: {agent_version}. Protocol: {protocol_version}");
						self.swarm.behaviour_mut().kademlia.remove_peer(&peer_id);
						self.swarm.behaviour_mut().blocked_peers.block_peer(peer_id);
					}
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
					trace!("[AutoNat] Inbound Probe: {:#?}", e);
				},
				autonat::Event::OutboundProbe(e) => {
					trace!("[AutoNat] Outbound Probe: {:#?}", e);
				},
				autonat::Event::StatusChanged { old, new } => {
					debug!("[AutoNat] Old status: {:#?}. New status: {:#?}", old, new);
					// check if went private or are private
					// if so, create reservation request with relay
					if new == NatStatus::Private || old == NatStatus::Private {
						info!("[AutoNat] Autonat says we're still private.");
						// Fat clients should always be in Kademlia client mode, no need to do NAT traversal
						if !self.event_loop_config.is_fat_client {
							// select a relay, try to dial it
							self.select_and_dial_relay();
						}
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
			SwarmEvent::Behaviour(BehaviourEvent::Ping(ping::Event { result, .. })) => {
				if let Ok(rtt) = result {
					let _ = metrics
						.record(MetricValue::PingLatency(rtt.as_millis() as f64))
						.await;
				}
			},
			SwarmEvent::Behaviour(BehaviourEvent::Upnp(event)) => match event {
				upnp::Event::NewExternalAddr(addr) => {
					trace!("[UPnP] New external address: {addr}");
				},
				upnp::Event::GatewayNotFound => {
					trace!("[UPnP] Gateway does not support UPnP");
				},
				upnp::Event::NonRoutableGateway => {
					trace!("[UPnP] Gateway is not exposed directly to the public Internet, i.e. it itself has a private IP address.");
				},
				upnp::Event::ExpiredExternalAddr(addr) => {
					trace!("[UPnP] Gateway address expired: {addr}");
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
					SwarmEvent::IncomingConnection { .. } => {
						metrics.count(MetricCounter::IncomingConnection).await;
					},
					SwarmEvent::IncomingConnectionError { .. } => {
						metrics.count(MetricCounter::IncomingConnectionError).await;
					},
					SwarmEvent::ExternalAddrConfirmed { address } => {
						info!(
							"External reachability confirmed on address: {}",
							address.to_string()
						);
					},
					SwarmEvent::ConnectionEstablished { peer_id, .. } => {
						metrics.count(MetricCounter::ConnectionEstablished).await;
						// Notify the connections we're waiting on that we've connected successfully
						if let Some(ch) = self.pending_swarm_events.remove(&peer_id) {
							_ = ch.send(Ok(()));
						}
						self.establish_relay_circuit(peer_id);
					},
					SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
						metrics.count(MetricCounter::OutgoingConnectionError).await;

						if let Some(peer_id) = peer_id {
							// Notify the connections we're waiting on an error has occurred
							if let libp2p::swarm::DialError::WrongPeerId { .. } = &error {
								if let Some(peer) =
									self.swarm.behaviour_mut().kademlia.remove_peer(&peer_id)
								{
									let removed_peer_id = peer.node.key.preimage();
									debug!("Removed peer {removed_peer_id} from the routing table");
								}
							}
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
			&mut self.pending_swarm_events,
			&mut self.active_blocks,
		)) {
			command.abort(eyre!(err));
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

	async fn handle_put_result(
		&mut self,
		key: RecordKey,
		stats: QueryStats,
		is_error: bool,
		metrics: Arc<impl Metrics>,
	) {
		let block_num = match key.clone().try_into() {
			Ok(DHTKey::Cell(block_num, _, _)) => block_num,
			Ok(DHTKey::Row(block_num, _)) => block_num,
			Err(error) => {
				warn!("Unable to cast Kademlia key to DHT key: {error}");
				return;
			},
		};
		if let Some(block) = self.active_blocks.get_mut(&block_num) {
			// Decrement record counter for this block
			block.remaining_counter -= 1;
			if is_error {
				block.error_counter += 1;
			} else {
				block.success_counter += 1;
			}

			block.time_stat = stats
				.duration()
				.as_ref()
				.map(Duration::as_secs)
				.unwrap_or_default();

			if block.remaining_counter == 0 {
				let success_rate = block.success_counter as f64 / block.total_count as f64;
				info!(
					"Cell upload success rate for block {block_num}: {}/{}. Duration: {}",
					block.success_counter, block.total_count, block.time_stat
				);
				_ = metrics
					.record(MetricValue::DHTPutSuccess(success_rate))
					.await;

				_ = metrics
					.record(MetricValue::DHTPutDuration(block.time_stat as f64))
					.await;
			}

			if self.event_loop_config.is_fat_client {
				// Remove local records for fat clients (memory optimization)
				debug!("Pruning local records on fat client");
				self.swarm.behaviour_mut().kademlia.remove_record(&key);
			}
		} else {
			debug!("Can't find block in the active blocks list")
		}
	}
}

#[cfg(test)]
mod tests {
	use crate::network::p2p::event_loop::DHTKey;
	use color_eyre::Result;
	use libp2p::kad::RecordKey;

	#[test]
	fn dht_key_parse_record_key() {
		let row_key: DHTKey = RecordKey::new(&"1:2").try_into().unwrap();
		assert_eq!(row_key, DHTKey::Row(1, 2));

		let cell_key: DHTKey = RecordKey::new(&"3:2:1").try_into().unwrap();
		assert_eq!(cell_key, DHTKey::Cell(3, 2, 1));

		let result: Result<DHTKey> = RecordKey::new(&"1:2:4:3").try_into();
		_ = result.unwrap_err();

		let result: Result<DHTKey> = RecordKey::new(&"123").try_into();
		_ = result.unwrap_err();
	}
}
