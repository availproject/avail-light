use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use async_std::stream::StreamExt;
use itertools::Either;
use rand::seq::SliceRandom;
use std::str;
use tokio::{
	sync::{mpsc, oneshot},
	time::{interval_at, Instant, Interval},
};
use void::Void;

use super::{
	client::{Command, NumSuccPut},
	Behaviour, BehaviourEvent, Event,
};

const PEER_ID: &str = "PeerID";
const MULTIADDRESS: &str = "Multiaddress";
const STATUS: &str = "Status";

use libp2p::{
	autonat::{Event as AutonatEvent, NatStatus},
	dcutr::{
		inbound::UpgradeError as InboundUpgradeError,
		outbound::UpgradeError as OutboundUpgradeError, Event as DcutrEvent,
	},
	identify::{Event as IdentifyEvent, Info},
	kad::{
		BootstrapOk, EntryView, GetRecordOk, InboundRequest, KademliaEvent, PeerRecord, QueryId,
		QueryResult,
	},
	mdns::Event as MdnsEvent,
	metrics::{Metrics, Recorder},
	multiaddr::Protocol,
	ping,
	relay::{
		inbound::stop::FatalUpgradeError as InboundStopFatalUpgradeError,
		outbound::hop::FatalUpgradeError as OutboundHopFatalUpgradeError,
	},
	swarm::{
		dial_opts::{DialOpts, PeerCondition},
		ConnectionError, ConnectionHandlerUpgrErr, SwarmEvent,
	},
	Multiaddr, PeerId, Swarm,
};
use uuid::Uuid;

use crate::telemetry::metrics::{MetricEvent, Metrics as AvailMetrics};
use tracing::{debug, error, info, trace};

#[derive(Debug)]
enum QueryChannel {
	GetRecord(oneshot::Sender<Result<PeerRecord>>),
	PutRecordBatch(mpsc::Sender<NumSuccPut>),
	Bootstrap(oneshot::Sender<Result<()>>),
}

struct RelayState {
	id: PeerId,
	address: Multiaddr,
	is_circuit_established: bool,
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

// BootstrapState keeps track of all things bootstrap ralated
struct BootstrapState {
	// referring to the initial bootstrap process,
	// one that runs when the Light Client node starts up
	is_startup_done: bool,
	timer: Interval,
}

// Query Details struct gives additional meaning and data to ongoing KAD queries
struct QueryDetails {
	// Batch ID field is only ever populated in case query is being utilized in batch
	// only present in case of batching
	batch_id: Option<Uuid>,
	// Status field is used when query came from a batch
	// can be used otherwise, but for the time being only used with batching
	status: Option<QueryStatus>,
	// Result sender channel is always present, for all queries
	res_sender: QueryChannel,
}

// Query Status enum represents current state of the ongoing KAD query
enum QueryStatus {
	Pending,
	Succeeded,
	Failed(anyhow::Error),
}

pub struct EventLoop {
	swarm: Swarm<Behaviour>,
	command_receiver: mpsc::Receiver<Command>,
	output_senders: Vec<mpsc::Sender<Event>>,
	pending_kad_queries: HashMap<QueryId, QueryDetails>,
	pending_kad_routing: HashMap<PeerId, oneshot::Sender<Result<()>>>,
	metrics: Metrics,
	avail_metrics: AvailMetrics,
	relay: RelayState,
	bootstrap: BootstrapState,
	kad_remove_local_record: bool,
}

type IoOrPing = Either<Either<std::io::Error, std::io::Error>, ping::Failure>;

type UpgradeOrPing = Either<Either<IoOrPing, void::Void>, ConnectionHandlerUpgrErr<std::io::Error>>;

type StopOrHop = Either<
	ConnectionHandlerUpgrErr<Either<InboundStopFatalUpgradeError, OutboundHopFatalUpgradeError>>,
	void::Void,
>;

type PingOrStopOrHop = Either<UpgradeOrPing, StopOrHop>;

type Upgrade = Either<
	ConnectionHandlerUpgrErr<Either<InboundUpgradeError, OutboundUpgradeError>>,
	Either<ConnectionHandlerUpgrErr<std::io::Error>, Void>,
>;

type PingFailureOrUpgradeError = Either<PingOrStopOrHop, Upgrade>;

impl EventLoop {
	pub fn new(
		swarm: Swarm<Behaviour>,
		command_receiver: mpsc::Receiver<Command>,
		metrics: Metrics,
		avail_metrics: AvailMetrics,
		relay_nodes: Vec<(PeerId, Multiaddr)>,
		periodic_bootstrap_interval: Duration,
		kad_remove_local_record: bool,
	) -> Self {
		Self {
			swarm,
			command_receiver,
			output_senders: Vec::new(),
			pending_kad_queries: Default::default(),
			pending_kad_routing: Default::default(),
			metrics,
			avail_metrics,
			relay: RelayState {
				id: PeerId::random(),
				address: Multiaddr::empty(),
				is_circuit_established: false,
				nodes: relay_nodes,
			},
			bootstrap: BootstrapState {
				is_startup_done: false,
				timer: interval_at(
					Instant::now() + periodic_bootstrap_interval,
					periodic_bootstrap_interval,
				),
			},
			kad_remove_local_record,
		}
	}

	pub async fn run(mut self) {
		loop {
			tokio::select! {
				event = self.swarm.next() => self.handle_event(event.expect("Swarm stream should be infinite")),
				command = self.command_receiver.recv() => match command {
					Some(c) => self.handle_command(c),
					// Command channel closed, thus shutting down the network event loop.
					None => return,
				},
				_ = self.bootstrap.timer.tick() => self.handle_periodic_bootstraps(),
			}
		}
	}

	// Notify function is used to send network events to all listeners
	// through send channels that are able to send, otherwise channel is discarded
	fn notify(&mut self, event: Event) {
		self.output_senders
			.retain(|tx| tx.try_send(event.clone()).is_ok());
	}

	fn handle_event(&mut self, event: SwarmEvent<BehaviourEvent, PingFailureOrUpgradeError>) {
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
						if let InboundRequest::PutRecord { source, record, .. } = request {
							let key = &record.as_ref().expect("msg").key;
							trace!("Inbound PUT request record key: {key:?}. Source: {source:?}",);

							_ = self
								.swarm
								.behaviour_mut()
								.kademlia
								.store_mut()
								.put(record.expect("msg"));
						}
					},
					KademliaEvent::OutboundQueryProgressed { id, result, .. } => match result {
						QueryResult::GetRecord(result) => match result {
							Ok(GetRecordOk::FoundRecord(record)) => {
								if let Some(QueryDetails {
									res_sender: QueryChannel::GetRecord(ch),
									..
								}) = self.pending_kad_queries.remove(&id)
								{
									_ = ch.send(Ok(record));
								}
							},
							Err(err) => {
								if let Some(QueryDetails {
									res_sender: QueryChannel::GetRecord(ch),
									..
								}) = self.pending_kad_queries.remove(&id)
								{
									_ = ch.send(Err(err.into()));
								}
							},
							_ => (),
						},
						QueryResult::PutRecord(result) => {
							// update this records ongoing Status Field accordingly, if there's any
							self.pending_kad_queries.entry(id).and_modify(|qd| {
								if let QueryDetails {
									status: Some(query_status),
									..
								} = qd
								{
									// Set status for ongoing KAD query based on result from PUT
									// TODO: Handle or log errors
									*query_status = match result {
										Ok(record) => {
											// check if removal of local records is set for fat clients memory optimization
											if self.kad_remove_local_record {
												self.swarm
													.behaviour_mut()
													.kademlia
													.remove_record(&record.key);
											}
											QueryStatus::Succeeded
										},
										Err(err) => QueryStatus::Failed(err.into()),
									}
								}
							});

							// count phase:
							let ids_to_remove = if let Some(QueryDetails {
								batch_id: Some(uuid),
								res_sender: QueryChannel::PutRecordBatch(ch),
								..
							}) = self.pending_kad_queries.get(&id)
							{
								// create iterator for all queries with same batch ID
								self.pending_kad_queries
									.iter()
									.filter(|(_, qd)| qd.batch_id == Some(*uuid))
									.all(|(_, qd)| {
										matches!(
											qd.status,
											Some(QueryStatus::Succeeded)
												| Some(QueryStatus::Failed(_))
										)
									})
									.then(|| {
										self.pending_kad_queries
											.iter()
											.filter(|(_, qd)| {
												matches!(
													qd.status,
													Some(QueryStatus::Succeeded)
														| Some(QueryStatus::Failed(_))
												)
											})
											.collect::<HashMap<&QueryId, &QueryDetails>>()
									})
									.and_then(|queries| {
										let success_count = queries
											.iter()
											.filter(|(_, qd)| {
												matches!(qd.status, Some(QueryStatus::Succeeded))
											})
											.count();
										// send result back
										_ = ch.send(NumSuccPut(success_count));

										let ids_to_remove = queries
											.iter()
											.map(|(&&id, _)| id)
											.collect::<Vec<QueryId>>();
										Some(ids_to_remove)
									})
							} else {
								None
							};

							if let Some(ids) = ids_to_remove {
								ids.iter().for_each(|id| {
									self.pending_kad_queries.remove(id);
								})
							}
						},
						QueryResult::Bootstrap(result) => match result {
							Ok(BootstrapOk {
								peer,
								num_remaining,
							}) => {
								trace!("BootstrapOK event. PeerID: {peer:?}. Num remaining: {num_remaining:?}.");
								if num_remaining == 0 {
									if let Some(QueryDetails {
										res_sender: QueryChannel::Bootstrap(ch),
										..
									}) = self.pending_kad_queries.remove(&id)
									{
										_ = ch.send(Ok(()));
										// we can say that the startup bootstrap is done here
										if self.bootstrap.is_startup_done == false {
											self.bootstrap.is_startup_done = true;
										}
									}
								}
							},
							Err(err) => {
								trace!("Bootstrap error event. Error: {err:?}.");
								if let Some(QueryDetails {
									res_sender: QueryChannel::Bootstrap(ch),
									..
								}) = self.pending_kad_queries.remove(&id)
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
						self.establish_relay_circuit(peer_id);

						// only interested in addresses with actual Multiaddresses
						// ones that contains the 'p2p' tag
						listen_addrs
							.into_iter()
							.filter(|a| a.to_string().contains(Protocol::P2p(peer_id.into()).tag()))
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

						if self.swarm.behaviour_mut().mdns.has_node(&peer_id) {
							self.swarm
								.behaviour_mut()
								.kademlia
								.remove_address(&peer_id, &multiaddr);
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
						// select a relay, try to dial it
						self.select_and_dial_relay();
					};
				},
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
							if self.relay.id == peer_id {
								self.relay.reset();
							}
						}
					},
					SwarmEvent::Dialing(peer_id) => debug!("Dialing {}", peer_id),
					_ => {},
				}
			},
		}
	}

	fn handle_command(&mut self, command: Command) {
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

				self.pending_kad_queries.insert(
					query_id,
					QueryDetails {
						batch_id: None,
						status: None,
						res_sender: QueryChannel::Bootstrap(sender),
					},
				);
			},
			Command::GetKadRecord { key, sender } => {
				let query_id = self.swarm.behaviour_mut().kademlia.get_record(key);

				self.pending_kad_queries.insert(
					query_id,
					QueryDetails {
						batch_id: None,
						status: None,
						res_sender: QueryChannel::GetRecord(sender),
					},
				);
			},
			Command::PutKadRecordBatch {
				records,
				quorum,
				sender,
			} => {
				// create unique batch ID
				let batch_id = Uuid::new_v4();

				for record in records.as_ref() {
					let query_id = self
						.swarm
						.behaviour_mut()
						.kademlia
						.put_record(record.to_owned(), quorum)
						.expect("Should be able to perform Kademlia PUT operation while batching");

					// insert batch queries into pending KAD queries
					self.pending_kad_queries.insert(
						query_id,
						QueryDetails {
							batch_id: Some(batch_id),
							status: Some(QueryStatus::Pending),
							res_sender: QueryChannel::PutRecordBatch(sender.clone()),
						},
					);
				}
			},
			Command::ReduceKademliaMapSize => {
				self.swarm
					.behaviour_mut()
					.kademlia
					.store_mut()
					.shrink_hashmap();
			},
			Command::NetworkObservabilityDump => {
				self.dump_routing_table_stats();
				self.dump_hash_map_block_stats();
			},
		}
	}

	fn dump_hash_map_block_stats(&mut self) {
		let mut occurence_map = HashMap::new();

		for record in self
			.swarm
			.behaviour_mut()
			.kademlia
			.store_mut()
			.records_iter()
		{
			let vec_key = record.0.to_vec();
			let record_key = str::from_utf8(&vec_key);

			let (block_num, _) = record_key
				.expect("unable to cast key to string")
				.split_once(':')
				.expect("unable to split the key string");

			let count = occurence_map.entry(block_num.to_string()).or_insert(0);
			*count += 1;
		}
		let mut sorted: Vec<(&String, &i32)> = occurence_map.iter().collect();
		sorted.sort_by(|a, b| a.0.cmp(b.0));
		for (block_number, cell_count) in sorted {
			trace!(
				"Number of cells in DHT for block {:?}: {}",
				block_number,
				cell_count
			);
		}
	}

	fn dump_routing_table_stats(&mut self) {
		let num_of_buckets = self.swarm.behaviour_mut().kademlia.kbuckets().count();
		debug!("Number of KBuckets: {:?} ", num_of_buckets);
		let mut table: String = "".to_owned();
		let mut total_peer_number: usize = 0;
		for bucket in self.swarm.behaviour_mut().kademlia.kbuckets() {
			total_peer_number += bucket.num_entries();
			for EntryView { node, status } in bucket.iter().map(|r| r.to_owned()) {
				let key = node.key.preimage().to_string();
				let value = format!("{:?}", node.value);
				let status = format!("{:?}", status);
				table.push_str(&format! {"{key: <55} | {value: <100} | {status: <10}\n"});
			}
		}

		let text = format!("Total number of peers in routing table: {total_peer_number}.");
		let header = format!("{PEER_ID: <55} | {MULTIADDRESS: <100} | {STATUS: <10}",);
		debug!("{text}\n{header}\n{table}");

		self.avail_metrics
			.record(MetricEvent::KadRoutingTablePeerNum(
				total_peer_number
					.try_into()
					.expect("unable to convert usize to u32"),
			));
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
			match self.swarm.listen_on(
				self.relay
					.address
					.clone()
					.with(Protocol::P2p(peer_id.into()))
					.with(Protocol::P2pCircuit),
			) {
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
				info!("Daling Relay: {id:?} succeded.", id = self.relay.id);
			},
			Err(e) => {
				// got an error while dialing,
				// beter select a new relay and try again
				self.relay.reset();
				error!(
					"Dialing Relay: {id:?}, produced an error: {e:?}",
					id = self.relay.id
				);
			},
		}
	}
}
