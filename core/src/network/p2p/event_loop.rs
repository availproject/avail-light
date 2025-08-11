use color_eyre::{eyre::eyre, Result};
use futures::StreamExt;
use libp2p::{
	autonat::{self, NatStatus},
	core::ConnectedPoint,
	identify::{self, Info},
	kad::{
		self, store::RecordStore, BootstrapOk, GetClosestPeersError, GetClosestPeersOk,
		GetRecordOk, InboundRequest, Mode, PutRecordOk, QueryId, QueryResult,
	},
	ping,
	swarm::SwarmEvent,
	Multiaddr, PeerId, StreamProtocol, Swarm,
};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
use std::{collections::HashMap, str::FromStr};
use tokio::sync::{
	mpsc::{UnboundedReceiver, UnboundedSender},
	oneshot,
};
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::{interval, Instant};
#[cfg(target_arch = "wasm32")]
use tokio_with_wasm::alias as tokio;
use tracing::{debug, info, trace, warn};
#[cfg(target_arch = "wasm32")]
use web_time::{Duration, Instant};

use super::{
	configuration::LibP2PConfig, Command, ConfigurableBehaviour, ConfigurableBehaviourEvent,
	OutputEvent, QueryChannel,
};
use crate::{
	network::p2p::{is_global_address, AgentVersion, KADEMLIA_PROTOCOL_BASE},
	shutdown::Controller,
	types::TimeToLive,
};

struct EventLoopConfig {
	// Used for checking protocol version
	is_fat_client: bool,
	kad_record_ttl: TimeToLive,
	is_local_test_mode: bool,
	// List of address patterns to filter out
	address_blacklist: Vec<String>,
}

#[derive(Debug)]
pub struct ConnectionEstablishedInfo {
	pub peer_id: PeerId,
	pub endpoint: ConnectedPoint,
	pub established_in: Duration,
	pub num_established: u32,
}

#[derive(Clone)]
struct EventCounter {
	start_time: Instant,
	event_count: u64,
	report_interval: Duration,
}

impl EventCounter {
	fn new(report_interval_seconds: u64) -> Self {
		EventCounter {
			start_time: Instant::now(),
			event_count: 0,
			report_interval: Duration::from_secs(report_interval_seconds),
		}
	}

	fn add_event(&mut self) {
		self.event_count += 1;
	}

	fn count_events(&mut self) -> f64 {
		let elapsed = self.start_time.elapsed();
		self.event_count as f64 / elapsed.as_secs_f64() * self.duration_secs()
	}

	fn reset_counter(&mut self) {
		self.event_count = 0;
	}

	fn duration_secs(&mut self) -> f64 {
		self.report_interval.as_secs_f64()
	}
}

pub struct EventLoop {
	pub swarm: Swarm<ConfigurableBehaviour>,
	command_receiver: UnboundedReceiver<Command>,
	pub(crate) event_sender: UnboundedSender<OutputEvent>,
	// Tracking Kademlia events
	pub pending_kad_queries: HashMap<QueryId, QueryChannel>,
	// Tracking swarm events (i.e. peer dialing)
	pub pending_swarm_events: HashMap<PeerId, oneshot::Sender<Result<ConnectionEstablishedInfo>>>,
	shutdown: Controller<String>,
	event_loop_config: EventLoopConfig,
	pub kad_mode: Mode,
}

impl EventLoop {
	#[allow(clippy::too_many_arguments)]
	pub(crate) fn new(
		cfg: LibP2PConfig,
		swarm: Swarm<ConfigurableBehaviour>,
		is_fat_client: bool,
		command_receiver: UnboundedReceiver<Command>,
		event_sender: UnboundedSender<OutputEvent>,
		shutdown: Controller<String>,
	) -> Self {
		Self {
			swarm,
			command_receiver,
			event_sender,
			pending_kad_queries: Default::default(),
			pending_swarm_events: Default::default(),
			shutdown,
			event_loop_config: EventLoopConfig {
				is_fat_client,
				kad_record_ttl: TimeToLive(cfg.kademlia.kad_record_ttl),
				is_local_test_mode: cfg.local_test_mode,
				address_blacklist: cfg.address_blacklist,
			},
			kad_mode: cfg.kademlia.operation_mode.into(),
		}
	}

	pub async fn run(mut self) {
		info!("Running P2P event loop");
		// shutdown will wait as long as this token is not dropped
		let _delay_token = self
			.shutdown
			.delay_token()
			.expect("There should not be any shutdowns at the begging of the P2P Event Loop");
		let mut event_counter = EventCounter::new(30);

		#[cfg(not(target_arch = "wasm32"))]
		let mut report_timer = interval(event_counter.report_interval);

		#[cfg(target_arch = "wasm32")]
		let mut next_tick = Instant::now() + event_counter.report_interval;

		loop {
			#[cfg(not(target_arch = "wasm32"))]
			tokio::select! {
				event = self.swarm.next() => {
					self.handle_event(event.expect("Swarm stream should be infinite")).await;
					event_counter.add_event();

					_ = self.event_sender.send(OutputEvent::Count);
				},
				command = self.command_receiver.recv() => match command {
					Some(c) => _ = (c)(&mut self),
					//
					None => {
						warn!("Command channel closed, exiting the network event loop");
						break;
					},
				},
				_ = report_timer.tick() => {
					debug!("Events per {}s: {:.2}", event_counter.duration_secs(), event_counter.count_events());
					event_counter.reset_counter();
				},
				// if the shutdown was triggered,
				// break the loop immediately, proceed to the cleanup phase
				_ = self.shutdown.triggered_shutdown() => {
					info!("Shutdown triggered, exiting the network event loop");
					break;
				}
			}

			#[cfg(target_arch = "wasm32")]
			let now = Instant::now();

			#[cfg(target_arch = "wasm32")]
			tokio::select! {
				event = self.swarm.next() => {
					self.handle_event(event.expect("Swarm stream should be infinite")).await;
					event_counter.add_event();

					_ = self.event_sender.send(OutputEvent::Count);
				},
				command = self.command_receiver.recv() => match command {
					Some(c) => _ = (c)(&mut self),
					//
					None => {
						warn!("Command channel closed, exiting the network event loop");
						break;
					},
				},
				_ = tokio::time::sleep(next_tick.checked_duration_since(now).unwrap_or_default()) => {
					debug!("Events per {}s: {:.2}", event_counter.duration_secs(), event_counter.count_events());
					event_counter.reset_counter();
					next_tick += event_counter.report_interval;
				},
				// if the shutdown was triggered,
				// break the loop immediately, proceed to the cleanup phase
				_ = self.shutdown.triggered_shutdown() => {
					info!("Shutdown triggered, exiting the network event loop");
					break;
				}
			}
		}
		self.disconnect_peers();
		info!("Exiting P2P event loop");
	}

	fn disconnect_peers(&mut self) {
		let connected_peers: Vec<PeerId> = self.swarm.connected_peers().cloned().collect();
		// close all active connections with other peers
		for peer in connected_peers {
			_ = self.swarm.disconnect_peer_id(peer);
		}
	}

	fn should_allow_peer(
		&mut self,
		peer_id: PeerId,
		agent_version: &str,
		protocols: Vec<StreamProtocol>,
		listen_addrs: Vec<Multiaddr>,
		kad_protocol_name: StreamProtocol,
	) -> Result<Vec<Multiaddr>> {
		// Parse and validate agent version
		let incoming_peer_agent_version =
			AgentVersion::from_str(agent_version).map_err(|e| eyre!(e))?;

		// Check if the release version is supported
		if !incoming_peer_agent_version.is_supported() {
			return Err(eyre!(
				"Unsupported release version: {}",
				incoming_peer_agent_version.release_version
			));
		}

		// Check if peer supports the correct Kademlia protocol
		if let Some(protocol) = protocols
			.iter()
			.find(|p| p.as_ref().starts_with(KADEMLIA_PROTOCOL_BASE))
		{
			if protocol.as_ref() != kad_protocol_name.as_ref() {
				return Err(eyre!("Incorrect peer Kademlia protocol name."));
			}
		}

		// Early exit if any address matches the blacklist filters
		for addr in &listen_addrs {
			let addr_str = addr.to_string();
			if self
				.event_loop_config
				.address_blacklist
				.iter()
				.any(|filter| addr_str.contains(filter))
			{
				return Err(eyre!("Peer {addr_str}/p2p/{peer_id} blacklisted."));
			}
		}

		// Filter and collect valid addresses
		let valid_addrs: Vec<Multiaddr> = listen_addrs
			.into_iter()
			.filter(|addr| self.event_loop_config.is_local_test_mode || is_global_address(addr))
			.collect();

		Ok(valid_addrs)
	}

	#[tracing::instrument(level = "trace", skip(self))]
	async fn handle_event(&mut self, event: SwarmEvent<ConfigurableBehaviourEvent>) {
		match event {
			SwarmEvent::Behaviour(ConfigurableBehaviourEvent::Kademlia(event)) => {
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
							_ = self.event_sender.send(OutputEvent::IncomingGetRecord);
						},
						InboundRequest::PutRecord { source, record, .. } => {
							_ = self.event_sender.send(OutputEvent::IncomingPutRecord);

							match record {
								Some(mut record) => {
									let ttl = &self.event_loop_config.kad_record_ttl;

									// Set TTL for all incoming records
									// TTL will be set to a lower value between the local TTL and incoming record TTL
									record.expires = record.expires.min(ttl.expires());
									if let Some(kad) = self.swarm.behaviour_mut().kademlia.as_mut()
									{
										_ = kad.store_mut().put(record);
									}
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
						// This event should not be automatically triggered because the mode changes are handled explicitly through the LC logic
						self.kad_mode = new_mode;
						_ = self.event_sender.send(OutputEvent::KadModeChange(new_mode));
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
						QueryResult::GetClosestPeers(result) => match result {
							Ok(GetClosestPeersOk { peers, .. }) => {
								let peer_addresses = collect_peer_addresses(peers);
								if let Some(QueryChannel::GetClosestPeer(ch)) =
									self.pending_kad_queries.remove(&id)
								{
									let _ = ch.send(Ok(peer_addresses.clone()));
								}
								// Notify about discovered peers even if there are no active addresses
								let _ = self.event_sender.send(OutputEvent::DiscoveredPeers {
									peers: peer_addresses,
								});
							},
							Err(err) => {
								// There will be peers even though the request timed out
								let GetClosestPeersError::Timeout { key: _, peers } = err.clone();
								let peer_addresses = collect_peer_addresses(peers);
								if let Some(QueryChannel::GetClosestPeer(ch)) =
									self.pending_kad_queries.remove(&id)
								{
									let _ = ch.send(Ok(peer_addresses.clone()));
								}
								// Notify about discovered peers even if there are no active addresses
								let _ = self.event_sender.send(OutputEvent::DiscoveredPeers {
									peers: peer_addresses,
								});
							},
						},
						QueryResult::PutRecord(Err(error)) => {
							match error {
								kad::PutRecordError::QuorumFailed { key, .. }
								| kad::PutRecordError::Timeout { key, .. } => {
									// Remove local records for fat clients (memory optimization)
									if self.event_loop_config.is_fat_client {
										trace!("Pruning local records on fat client");
										if let Some(kad) =
											self.swarm.behaviour_mut().kademlia.as_mut()
										{
											kad.remove_record(&key)
										}
									}

									_ = self.event_sender.send(OutputEvent::PutRecordFailed {
										record_key: key,
										query_stats: stats,
									});
								},
							}
						},

						QueryResult::PutRecord(Ok(PutRecordOk { key })) => {
							// Remove local records for fat clients (memory optimization)
							if self.event_loop_config.is_fat_client {
								trace!("Pruning local records on fat client");
								if let Some(kad) = self.swarm.behaviour_mut().kademlia.as_mut() {
									kad.remove_record(&key)
								}
							}

							_ = self.event_sender.send(OutputEvent::PutRecordSuccess {
								record_key: key,
								query_stats: stats,
							});
						},
						QueryResult::Bootstrap(result) => match result {
							Ok(BootstrapOk {
								peer,
								num_remaining,
							}) => {
								debug!("BootstrapOK event. PeerID: {peer:?}. Num remaining: {num_remaining:?}.");
								if num_remaining == 0 {
									info!("Bootstrap completed.");
								}
							},
							Err(err) => {
								debug!("Bootstrap error event. Error: {err:?}.");
							},
						},
						_ => {},
					},
				}
			},
			SwarmEvent::Behaviour(ConfigurableBehaviourEvent::Identify(event)) => {
				match event {
					identify::Event::Received {
						peer_id,
						info:
							Info {
								listen_addrs,
								agent_version,
								protocol_version: _,
								protocols,
								..
							},
						connection_id: _,
					} => {
						trace!(agent_version,  "Identity received from {peer_id:?}, listen address: {listen_addrs:?}, protocols: {protocols:?}");

						// KAD Discovery process
						let kad_protocol_name = self
							.swarm
							.behaviour_mut()
							.kademlia
							.as_ref()
							.map(|kad| kad.protocol_names()[0].clone());

						if let Some(kad_protocol_name) = kad_protocol_name {
							match self.should_allow_peer(
								peer_id,
								&agent_version,
								protocols,
								listen_addrs,
								kad_protocol_name,
							) {
								Ok(addreses) => {
									if let Some(kad) = self.swarm.behaviour_mut().kademlia.as_mut()
									{
										for addr in &addreses {
											trace!(
												"Adding peer {addr}/{peer_id} to routing table."
											);
											kad.add_address(&peer_id, addr.clone());
										}
									}
								},
								Err(e) => {
									debug!("{e}");
									self.swarm
										.behaviour_mut()
										.blocked_peers
										.as_mut()
										.unwrap()
										.block_peer(peer_id);
								},
							}
						}
					},
					identify::Event::Sent {
						peer_id,
						connection_id: _,
					} => {
						trace!("Identity Sent event to: {peer_id:?}");
					},
					identify::Event::Pushed { peer_id, .. } => {
						trace!("Identify Pushed event. PeerId: {peer_id:?}");
					},
					identify::Event::Error {
						peer_id,
						error,
						connection_id: _,
					} => {
						trace!("Identify Error event. PeerId: {peer_id:?}. Error: {error:?}");
					},
				}
			},
			SwarmEvent::Behaviour(ConfigurableBehaviourEvent::AutoNat(event)) => match event {
				autonat::Event::InboundProbe(e) => {
					trace!("[AutoNat] Inbound Probe: {:#?}", e);
				},
				autonat::Event::OutboundProbe(e) => {
					trace!("[AutoNat] Outbound Probe: {:#?}", e);
				},
				autonat::Event::StatusChanged { old, new } => {
					debug!("[AutoNat] Old status: {:#?}. New status: {:#?}", old, new);
					if new == NatStatus::Private || old == NatStatus::Private {
						info!("[AutoNat] Autonat says we're still private.");
					};
				},
			},
			SwarmEvent::Behaviour(ConfigurableBehaviourEvent::Ping(ping::Event {
				peer,
				result,
				..
			})) => {
				if let Ok(rtt) = result {
					_ = self.event_sender.send(OutputEvent::Ping { peer, rtt });
				}
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
					},
					SwarmEvent::IncomingConnection { .. } => {
						_ = self.event_sender.send(OutputEvent::IncomingConnection);
					},
					SwarmEvent::IncomingConnectionError { .. } => {
						_ = self.event_sender.send(OutputEvent::IncomingConnectionError);
					},
					SwarmEvent::ExternalAddrConfirmed { address } => {
						info!(
							"External reachability confirmed on address: {}",
							address.to_string()
						);
						if is_global_address(&address) {
							info!(
								"Public reachability confirmed on address: {}",
								address.to_string()
							);
						};

						_ = self
							.event_sender
							.send(OutputEvent::ExternalMultiaddressUpdate(address));
					},
					SwarmEvent::ConnectionEstablished {
						peer_id,
						endpoint,
						established_in,
						num_established,
						..
					} => {
						_ = self.event_sender.send(OutputEvent::EstablishedConnection);

						// Notify the connections we're waiting on that we've connected successfully
						if let Some(ch) = self.pending_swarm_events.remove(&peer_id) {
							_ = ch.send(Ok(ConnectionEstablishedInfo {
								peer_id,
								endpoint,
								established_in,
								num_established: num_established.into(),
							}));
						}
					},
					SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
						_ = self.event_sender.send(OutputEvent::OutgoingConnectionError);

						if let Some(peer_id) = peer_id {
							// Notify the connections we're waiting on an error has occurred
							if let libp2p::swarm::DialError::WrongPeerId { .. } = &error {
								if let Some(kad) = self.swarm.behaviour_mut().kademlia.as_mut() {
									if let Some(peer) = kad.remove_peer(&peer_id) {
										let removed_peer_id = peer.node.key.preimage();
										debug!("Removed peer {removed_peer_id} from the routing table. Cause: {error}");
									}
								}
							}
							if let Some(ch) = self.pending_swarm_events.remove(&peer_id) {
								_ = ch.send(Err(error.into()));
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
}

fn collect_peer_addresses(peers: Vec<kad::PeerInfo>) -> Vec<(PeerId, Vec<Multiaddr>)> {
	peers
		.iter()
		.filter(|peer_info| !peer_info.addrs.is_empty())
		.map(|peer_info| {
			let peer_id = peer_info.peer_id;
			let addresses = peer_info.addrs.clone();
			(peer_id, addresses)
		})
		.collect()
}
