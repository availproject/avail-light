use color_eyre::{eyre::eyre, Result};
use futures::StreamExt;
use libp2p::{
	autonat::{self, NatStatus},
	core::ConnectedPoint,
	identify::{self, Info},
	kad::{
		self, store::RecordStore, BootstrapOk, GetClosestPeersError, GetClosestPeersOk,
		GetRecordOk, InboundRequest, Mode, PutRecordOk, QueryId, QueryResult, RecordKey,
	},
	multiaddr::Protocol,
	ping,
	swarm::SwarmEvent,
	Multiaddr, PeerId, Swarm,
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
	network::p2p::{is_multiaddr_global, AgentVersion},
	shutdown::Controller,
	types::TimeToLive,
};

struct EventLoopConfig {
	// Used for checking protocol version
	is_fat_client: bool,
	kad_record_ttl: TimeToLive,
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
			},
			kad_mode: cfg.kademlia.operation_mode.into(),
		}
	}

	pub async fn run(mut self) {
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
	}

	fn disconnect_peers(&mut self) {
		let connected_peers: Vec<PeerId> = self.swarm.connected_peers().cloned().collect();
		// close all active connections with other peers
		for peer in connected_peers {
			_ = self.swarm.disconnect_peer_id(peer);
		}
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
									_ = self
										.swarm
										.behaviour_mut()
										.kademlia
										.as_mut()
										.map(|kad| kad.store_mut().put(record));
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
								protocol_version,
								protocols,
								..
							},
						connection_id: _,
					} => {
						trace!(
						"Identity Received from: {peer_id:?} on listen address: {listen_addrs:?}"
					);

						if let Some(kad) = self.swarm.behaviour_mut().kademlia.as_mut() {
							let incoming_peer_agent_version =
								match AgentVersion::from_str(&agent_version) {
									Ok(agent) => agent,
									Err(e) => {
										debug!("Error parsing incoming agent version: {e}");
										return;
									},
								};

							if !incoming_peer_agent_version.is_supported() {
								debug!(
									"Unsupported release version: {}",
									incoming_peer_agent_version.release_version
								);
								kad.remove_peer(&peer_id);
								return;
							}

							let protocol = kad.protocol_names()[0].clone();
							if protocols.contains(&protocol) {
								listen_addrs
									.into_iter()
									// Filter out the loopback addresses
									.filter(|addr| !is_localhost(addr))
									.for_each(|addr| {
										trace!("Adding peer {peer_id} to routing table.");
										kad.add_address(&peer_id, addr);
									});
							} else {
								// Block and remove non-compatible peers
								debug!("Removing and blocking peer from routing table. Peer: {peer_id}. Agent: {agent_version}. Protocol: {protocol_version}");
								kad.remove_peer(&peer_id);
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
					// TODO: this should be automated with the addition of the new AutoNAT v2
					SwarmEvent::NewListenAddr {
						listener_id,
						address,
					} => {
						if is_localhost(&address) {
							self.swarm.remove_listener(listener_id);
							debug!("Removed localhost listener from: {:?}", address);
						} else {
							debug!("Local node is listening on {:?}", address);
						}
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
						if is_multiaddr_global(&address) {
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
								if let Some(Some(peer)) = self
									.swarm
									.behaviour_mut()
									.kademlia
									.as_mut()
									.map(|kad| kad.remove_peer(&peer_id))
								{
									let removed_peer_id = peer.node.key.preimage();
									debug!("Removed peer {removed_peer_id} from the routing table. Cause: {error}");
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

fn is_localhost(addr: &Multiaddr) -> bool {
	for protocol in addr.iter() {
		// Check IPv4 addresses in the 127.x.x.x range (loopback range)
		match protocol {
			Protocol::Ip4(ip) => {
				if ip.is_loopback() {
					return true;
				}
			},
			// Check IPv6 localhost (::1)
			Protocol::Ip6(ip) => {
				if ip.is_loopback() {
					return true;
				}
			},
			// Check DNS entries for localhost (case insensitive)
			Protocol::Dns(host)
			| Protocol::Dns4(host)
			| Protocol::Dns6(host)
			| Protocol::Dnsaddr(host) => {
				if host.to_lowercase() == "localhost" {
					return true;
				}
			},
			_ => continue,
		}
	}
	false
}

#[cfg(test)]
mod tests {
	use crate::network::p2p::event_loop::DHTKey;
	use color_eyre::Result;
	use libp2p::{kad::RecordKey, Multiaddr};
	use std::str::FromStr;

	use super::*;

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

	#[test]
	fn test_ipv4_localhost() {
		// Test simple address with default port
		let addr = Multiaddr::from_str("/ip4/127.0.0.1/tcp/8080").unwrap();
		assert!(is_localhost(&addr));

		// Test localhost with different ports
		let addrs_with_ports = vec![
			"/ip4/127.0.0.1/tcp/0",
			"/ip4/127.0.0.1/tcp/80",
			"/ip4/127.0.0.1/tcp/443",
			"/ip4/127.0.0.1/tcp/8000",
			"/ip4/127.0.0.1/tcp/65535",
			"/ip4/127.0.0.1/udp/53",
			"/ip4/127.0.0.1/sctp/9899",
		];

		for addr_str in addrs_with_ports {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(is_localhost(&addr), "Failed for: {}", addr_str);
		}

		// Test localhost with different protocols
		let addrs_with_protocols = vec![
			"/ip4/127.0.0.1/tcp/8080/http",
			"/ip4/127.0.0.1/tcp/443/tls/ws",
			"/ip4/127.0.0.1/tcp/8080/ws",
		];

		for addr_str in addrs_with_protocols {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(is_localhost(&addr), "Failed for: {}", addr_str);
		}

		// Test localhost range
		let test_cases = vec![
			"/ip4/127.0.0.2/tcp/8080",
			"/ip4/127.0.1.1/tcp/8080",
			"/ip4/127.1.0.1/tcp/8080",
			"/ip4/127.255.255.255/tcp/8080",
			"/ip4/127.1.2.3/tcp/8080",
		];

		for addr_str in test_cases {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				is_localhost(&addr),
				"Should detect localhost for: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_ipv6_localhost() {
		let test_addrs = vec![
			"/ip6/::1/tcp/8080",
			"/ip6/::1/tcp/0",
			"/ip6/::1/udp/53",
			"/ip6/::1/tcp/8080/http",
			"/ip6/::1/tcp/443/tls",
		];

		for addr_str in test_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(is_localhost(&addr), "Failed for: {}", addr_str);
		}
	}

	#[test]
	fn test_dns_localhost() {
		let test_addrs = vec![
			"/dns/localhost/tcp/8080",
			"/dns4/localhost/tcp/8080",
			"/dns6/localhost/tcp/8080",
			"/dnsaddr/localhost/tcp/8080",
		];

		for addr_str in test_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(is_localhost(&addr), "Failed for: {}", addr_str);
		}
	}

	#[test]
	fn test_non_localhost_ipv4() {
		let test_addrs = vec![
			"/ip4/0.0.0.0/tcp/8080",       // All interfaces
			"/ip4/192.168.1.1/tcp/8080",   // Private network
			"/ip4/10.0.0.1/tcp/8080",      // Private network
			"/ip4/172.16.0.1/tcp/8080",    // Private network
			"/ip4/8.8.8.8/tcp/53",         // Public DNS
			"/ip4/1.1.1.1/tcp/53",         // Cloudflare DNS
			"/ip4/203.0.113.1/tcp/80",     // Test network
			"/ip4/255.255.255.255/tcp/80", // Broadcast
		];

		for addr_str in test_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				!is_localhost(&addr),
				"Incorrectly identified as localhost: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_non_localhost_ipv6() {
		let test_addrs = vec![
			"/ip6/::/tcp/8080",                 // All interfaces
			"/ip6/2001:db8::1/tcp/8080",        // Documentation range
			"/ip6/fe80::1/tcp/8080",            // Link-local
			"/ip6/2001:4860:4860::8888/tcp/53", // Google DNS
			"/ip6/2606:4700:4700::1111/tcp/53", // Cloudflare DNS
			"/ip6/fc00::1/tcp/8080",            // Unique local
			"/ip6/ff02::1/tcp/8080",            // Multicast
		];

		for addr_str in test_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				!is_localhost(&addr),
				"Incorrectly identified as localhost: {}",
				addr_str
			);
		}
	}

	#[test]
	fn test_non_localhost_dns() {
		let test_addrs = vec![
			"/dns/example.com/tcp/80",
			"/dns4/google.com/tcp/443",
			"/dns6/ipv6.google.com/tcp/443",
			"/dnsaddr/bootstrap.libp2p.io/tcp/443",
			"/dns/peer.example.org/tcp/4001",
		];

		for addr_str in test_addrs {
			let addr = Multiaddr::from_str(addr_str).unwrap();
			assert!(
				!is_localhost(&addr),
				"Incorrectly identified as localhost: {}",
				addr_str
			);
		}
	}
}
