use anyhow::Result;
use futures::{future, FutureExt, StreamExt};
use itertools::Either;
use libp2p::{
	autonat::{Event as AutonatEvent, NatStatus},
	dcutr::{
		inbound::UpgradeError as InboundUpgradeError,
		outbound::UpgradeError as OutboundUpgradeError, Event as DcutrEvent,
	},
	identify::{Event as IdentifyEvent, Info},
	kad::{
		BootstrapOk, GetRecordOk, InboundRequest, KademliaEvent, PeerRecord, QueryId, QueryResult,
	},
	mdns::Event as MdnsEvent,
	multiaddr::Protocol,
	relay::{
		inbound::stop::FatalUpgradeError as InboundStopFatalUpgradeError,
		outbound::hop::FatalUpgradeError as OutboundHopFatalUpgradeError,
	},
	swarm::{
		dial_opts::{DialOpts, PeerCondition},
		ConnectionError, StreamUpgradeError, SwarmEvent,
	},
	Multiaddr, PeerId, Swarm,
};
use rand::seq::SliceRandom;
use std::str;
use std::{collections::HashMap, time::Duration};
use tokio::{
	sync::{
		mpsc::{self},
		oneshot,
	},
	time::{interval_at, Instant, Interval},
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info, trace};

use super::{client::Command, Behaviour, BehaviourEvent, DHTPutSuccess};

#[derive(Debug)]
enum QueryChannel {
	GetRecord(oneshot::Sender<Result<PeerRecord>>),
	PutRecordBatch(mpsc::Sender<DHTPutSuccess>),
	Bootstrap(oneshot::Sender<Result<()>>),
}

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
	command_receiver: mpsc::Receiver<Command>,
	pending_kad_queries: HashMap<QueryId, QueryChannel>,
	pending_kad_routing: HashMap<PeerId, oneshot::Sender<Result<()>>>,
	pending_swarm_events: HashMap<PeerId, oneshot::Sender<Result<()>>>,
	relay: RelayState,
	bootstrap: BootstrapState,
	kad_remove_local_record: bool,
}

type IoError = Either<
	Either<Either<Either<std::io::Error, std::io::Error>, void::Void>, void::Void>,
	void::Void,
>;

type StopOrHopError = Either<
	StreamUpgradeError<Either<InboundStopFatalUpgradeError, OutboundHopFatalUpgradeError>>,
	void::Void,
>;

type InOrOutError =
	Either<StreamUpgradeError<Either<InboundUpgradeError, OutboundUpgradeError>>, void::Void>;

type IoStopOrHopError = Either<IoError, StopOrHopError>;

type StreamError = Either<IoStopOrHopError, InOrOutError>;

impl EventLoop {
	pub fn new(
		swarm: Swarm<Behaviour>,
		command_receiver: mpsc::Receiver<Command>,
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
	async fn handle_event(&mut self, event: SwarmEvent<BehaviourEvent, StreamError>) {
		match event {
			SwarmEvent::Behaviour(BehaviourEvent::Kademlia(event)) => {
				match event {
					KademliaEvent::RoutingUpdated {
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
					KademliaEvent::RoutablePeer { peer, address } => {
						trace!("RoutablePeer. Peer: {peer:?}.  Address: {address:?}");
					},
					KademliaEvent::UnroutablePeer { peer } => {
						trace!("UnroutablePeer. Peer: {peer:?}");
					},
					KademliaEvent::PendingRoutablePeer { peer, address } => {
						trace!("Pending routablePeer. Peer: {peer:?}.  Address: {address:?}");
					},
					KademliaEvent::InboundRequest { request } => {
						trace!("Inbound request: {:?}", request);
						if let InboundRequest::PutRecord { source, record, .. } = request {
							let key = &record.as_ref().expect("No record found").key;
							trace!("Inbound PUT request record key: {key:?}. Source: {source:?}",);

							_ = self
								.swarm
								.behaviour_mut()
								.kademlia
								.store_mut()
								.put(record.expect("No record found"));
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
				}
			},
			SwarmEvent::Behaviour(BehaviourEvent::Identify(event)) => {
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
					IdentifyEvent::Sent { peer_id } => {
						trace!("Identity Sent event to: {peer_id:?}");
					},
					IdentifyEvent::Pushed { peer_id } => {
						trace!("Identify Pushed event. PeerId: {peer_id:?}");
					},
					IdentifyEvent::Error { peer_id, error } => {
						trace!("Identify Error event. PeerId: {peer_id:?}. Error: {error:?}");
					},
				}
			},
			SwarmEvent::Behaviour(BehaviourEvent::Mdns(event)) => match event {
				MdnsEvent::Discovered(addrs_list) => {
					for (peer_id, multiaddr) in addrs_list {
						trace!("MDNS got peer with ID: {peer_id:#?} and Address: {multiaddr:#?}");
						self.swarm
							.behaviour_mut()
							.kademlia
							.add_address(&peer_id, multiaddr);
					}
				},
				MdnsEvent::Expired(addrs_list) => {
					for (peer_id, multiaddr) in addrs_list {
						trace!("MDNS got expired peer with ID: {peer_id:#?} and Address: {multiaddr:#?}");

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
					trace!("AutoNat Inbound Probe: {:#?}", e);
				},
				AutonatEvent::OutboundProbe(e) => {
					trace!("AutoNat Outbound Probe: {:#?}", e);
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
				trace! {"Relay Client Event: {event:#?}"};
			},
			SwarmEvent::Behaviour(BehaviourEvent::Dcutr(event)) => match event {
				DcutrEvent::RemoteInitiatedDirectConnectionUpgrade {
					remote_peer_id,
					remote_relayed_addr,
				} => {
					trace!("Remote with ID: {remote_peer_id:#?} initiated Direct Connection Upgrade through address: {remote_relayed_addr:#?}");
				},
				DcutrEvent::InitiatedDirectConnectionUpgrade {
					remote_peer_id,
					local_relayed_addr,
				} => {
					trace!("Local node initiated Direct Connection Upgrade with remote: {remote_peer_id:#?} on address: {local_relayed_addr:#?}");
				},
				DcutrEvent::DirectConnectionUpgradeSucceeded { remote_peer_id } => {
					trace!("Hole punching succeeded with: {remote_peer_id:#?}")
				},
				DcutrEvent::DirectConnectionUpgradeFailed {
					remote_peer_id,
					error,
				} => {
					trace!("Hole punching failed with: {remote_peer_id:#?}. Error: {error:#?}");
				},
			},
			swarm_event => {
				match swarm_event {
					SwarmEvent::NewListenAddr { address, .. } => {
						info!("Local node is listening on {:?}", address);
					},
					SwarmEvent::ConnectionClosed {
						peer_id,
						endpoint,
						num_established,
						cause,
						..
					} => {
						trace!("Connection closed. PeerID: {peer_id:?}. Address: {:?}. Num established: {num_established:?}. Cause: {cause:?}", endpoint.get_remote_address());

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

	async fn handle_command(&mut self, command: Command) {
		match command {
			Command::StartListening {
				addr,
				response_sender: sender,
			} => {
				_ = match self.swarm.listen_on(addr) {
					Ok(_) => sender.send(Ok(())),
					Err(e) => sender.send(Err(e.into())),
				}
			},
			Command::AddAddress {
				peer_id,
				peer_addr,
				response_sender: sender,
			} => {
				self.swarm
					.behaviour_mut()
					.kademlia
					.add_address(&peer_id, peer_addr);

				self.pending_kad_routing.insert(peer_id, sender);
			},
			Command::DialAddress {
				peer_id,
				peer_addr,
				response_sender: sender,
			} => {
				let res = self.swarm.dial(
					DialOpts::peer_id(peer_id)
						.addresses(vec![peer_addr])
						.build(),
				);
				if let Err(e) = res {
					_ = sender.send(Err(e.into()));
					return;
				}
				self.pending_swarm_events.insert(peer_id, sender);
			},
			Command::Bootstrap {
				response_sender: sender,
			} => {
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
				response_sender: sender,
			} => {
				let query_id = self.swarm.behaviour_mut().kademlia.get_record(key);

				self.pending_kad_queries
					.insert(query_id, QueryChannel::GetRecord(sender));
			},
			Command::PutKadRecordBatch {
				records,
				quorum,
				response_sender: chunk_success_sender,
			} => {
				// create channels to track individual PUT results needed for success count
				let (put_result_tx, put_result_rx) = mpsc::channel::<DHTPutSuccess>(records.len());

				// spawn new task that waits and count all successful put queries from this batch,
				// but don't block event_loop
				tokio::spawn(
					<ReceiverStream<DHTPutSuccess>>::from(put_result_rx)
						// consider only while receiving single successful results
						.filter(|item| future::ready(item == &DHTPutSuccess::Single))
						.count()
						.map(DHTPutSuccess::Batch)
						// send back counted successful puts
						// signal back that this chunk of records is done
						.map(|successful_puts| chunk_success_sender.send(successful_puts)),
				);

				// go record by record and dispatch put requests through KAD
				for record in records {
					let query_id = self
						.swarm
						.behaviour_mut()
						.kademlia
						.put_record(record, quorum)
						.expect("Unable to perform batch Kademlia PUT operation.");
					// insert query id into pending KAD requests map
					self.pending_kad_queries.insert(
						query_id,
						QueryChannel::PutRecordBatch(put_result_tx.clone()),
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
			Command::CountDHTPeers { response_sender } => {
				let mut total_peers: usize = 0;
				for bucket in self.swarm.behaviour_mut().kademlia.kbuckets() {
					total_peers += bucket.num_entries();
				}
				_ = response_sender.send(total_peers);
			},
			Command::GetCellsInDHTPerBlock { response_sender } => {
				let mut occurrence_map = HashMap::new();

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

					let count = occurrence_map.entry(block_num.to_string()).or_insert(0);
					*count += 1;
				}
				let mut sorted: Vec<(&String, &i32)> = occurrence_map.iter().collect();
				sorted.sort_by(|a, b| a.0.cmp(b.0));
				for (block_number, cell_count) in sorted {
					trace!(
						"Number of cells in DHT for block {:?}: {}",
						block_number,
						cell_count
					);
				}

				_ = response_sender.send(Ok(()));
			},
			Command::GetMultiaddress { response_sender } => {
				let last_address = self.swarm.external_addresses().last();
				_ = response_sender.send(last_address.cloned());
			},
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
