extern crate anyhow;
extern crate async_std;
extern crate ed25519_dalek;
extern crate ipfs_embed;
extern crate libipld;
extern crate rand;
extern crate rocksdb;
extern crate tempdir;
extern crate tokio;

use std::{
	collections::{BTreeMap, HashMap},
	convert::TryFrom,
	str::FromStr,
	sync::{
		mpsc::{Receiver, SyncSender},
		Arc, Mutex,
	},
	time::Duration,
};

use async_std::stream::StreamExt;
use ipfs_embed::{
	Cid, DefaultParams as IPFSDefaultParams, Ipfs, Keypair, Multiaddr, NetworkConfig, PeerId,
	PublicKey, SecretKey, StorageConfig, ToLibp2p,
};
use kate_recovery::com::{reconstruct_app_extrinsics, Cell};
use libipld::Ipld;
use rocksdb::{DBWithThreadMode, SingleThreaded};

use crate::{
	data::{
		construct_matrix, decode_block_cid_ask_message, decode_block_cid_fact_message,
		extract_block, extract_cell, extract_links, prepare_block_cid_ask_message,
		prepare_block_cid_fact_message, push_matrix,
	},
	rpc::get_all_cells,
	types::{BlockCidPair, ClientMsg, Event},
};

#[tokio::main]
pub async fn run_client(
	cfg: super::types::RuntimeConfig,
	block_cid_store: Arc<DBWithThreadMode<SingleThreaded>>,
	block_rx: Receiver<ClientMsg>,
	self_info_tx: SyncSender<(PeerId, Multiaddr)>,
	destroy_rx: Receiver<bool>,
	cell_query_rx: Receiver<crate::types::CellContentQueryPayload>,
) -> anyhow::Result<(), Box<dyn std::error::Error + Send + Sync>> {
	let ipfs = make_client(cfg.ipfs_seed, cfg.ipfs_port, &cfg.ipfs_path).await?;
	let pin = ipfs.create_temp_pin()?;

	// inform invoker about self
	self_info_tx.send((ipfs.local_peer_id(), ipfs.listeners()[0].clone()))?;

	// bootstrap client with non-empty set of
	// application clients
	if cfg.bootstraps.len() > 0 {
		ipfs.bootstrap(
			&cfg.bootstraps
				.into_iter()
				.map(|(a, b)| (PeerId::from_str(&a).unwrap(), b))
				.collect::<Vec<(_, _)>>()[..],
		)
		.await?;
	}

	let ipfs_0: Ipfs<IPFSDefaultParams> = ipfs.clone();
	let block_cid_store_2 = block_cid_store.clone();
	tokio::task::spawn(async move {
		for query in cell_query_rx {
			match get_block_cid_entry(block_cid_store_2.clone(), query.block as i128) {
				Some(v) => {
					match ipfs_0.contains(&v.cid) {
						Ok(have) => {
							if !have {
								log::info!("block CID {:?} not present in local client", v.cid);
								continue;
							}

							match ipfs_0.get(&v.cid) {
								Ok(v) => {
									let dec_mat: Ipld;
									if let Ok(v) = v.ipld() {
										dec_mat = v;
									} else {
										log::info!("error: failed to IPLD decode data matrix");
										continue;
									}

									match dec_mat {
										Ipld::StringMap(map) => {
											let map: BTreeMap<String, Ipld> = map;
											let block = if let Some(data) = map.get("block") {
												extract_block(data)
											} else {
												None
											};

											if block == None {
												log::info!("error: failed to extract block number from message");
												continue;
											}

											let block = block.unwrap(); // safe to do so !
											if block as u64 != query.block {
												// just make it sure that everything is good
												log::info!("error: expected {} but received {} after CID based look up", query.block, block);
												continue;
											}

											let cids = if let Some(data) = map.get("columns") {
												extract_links(data)
											} else {
												None
											};

											if cids == None {
												log::info!("error: failed to extract column CIDs from message");
												continue;
											}

											let cids = cids.unwrap(); // safe to do so !
											if query.col as usize >= cids.len() {
												// ensure indexing into vector is good move
												log::info!("error: queried column {}, though max available columns {}", query.col, cids.len());
												continue;
											}

											let col_cid = cids[query.col as usize];
											if col_cid == None {
												log::info!("error: failed to extract respective column CID");
												continue;
											}

											let col_cid = col_cid.unwrap(); // safe to do so !

											match ipfs_0.get(&col_cid) {
												Ok(v) => {
													let dec_col: Ipld;
													if let Ok(v) = v.ipld() {
														dec_col = v;
													} else {
														log::info!("error: failed to IPLD decode data matrix column");
														continue;
													}

													let row_cids: Vec<Option<Cid>>;
													if let Some(v) = extract_links(&dec_col) {
														row_cids = v;
													} else {
														log::info!("error: failed to extract row CIDs from message");
														continue;
													}

													if query.row as usize >= row_cids.len() {
														// ensure indexing into vector is good move
														log::info!("error: queried row {}, though max available rows {}", query.row, row_cids.len());
														continue;
													}

													let row_cid = row_cids[query.row as usize];
													if row_cid == None {
														log::info!("error: failed to extract respective row CID");
														continue;
													}

													// safe to do so !
													let row_cid = row_cid.unwrap();

													match ipfs_0.get(&row_cid) {
														Ok(v) => {
															let dec_cell: Ipld;
															if let Ok(v) = v.ipld() {
																dec_cell = v;
															} else {
																log::info!("error: failed to IPLD decode data matrix cell");
																continue;
															}

															// respond back with content of cell
															if let Err(_) = query
																.res_chan
																.try_send(extract_cell(&dec_cell))
															{
																log::info!("error: failed to respond back to querying party");
															}
														},
														Err(_) => {
															log::info!("error: failed to get data matrix cell from storage");
														},
													};
												},
												Err(_) => {
													log::info!("error: failed to get data matrix column from storage");
												},
											};
										},
										_ => {},
									};
								},
								Err(_) => {
									log::info!("error: failed to get data matrix from storage");
								},
							};
						},
						Err(_) => {
							log::info!("block CID {:?} not present in local client", v.cid);
						},
					};
				},
				None => {
					log::info!("error: no CID entry found for block {}", query.block);
					if let Err(_) = query.res_chan.try_send(None) {
						log::info!("error: failed to respond back to querying party");
					}
				},
			}
		}
	});

	// // block to CID mapping to locally kept here ( in thead safe manner ! )

	// broadcast fact i.e. some block number is mapped to some
	// Cid ( of respective block data matrix ) to all peers subscribed
	let mut fact_topic = ipfs.subscribe("topic/block_cid_fact").unwrap();
	// ask over gossip network which Cid is associated with which block number
	// and expect some one, who knows, will answer it
	let mut ask_topic = ipfs.subscribe("topic/block_cid_ask").unwrap();

	// listen for messages received on fact topic !
	let block_cid_store_0 = block_cid_store.clone();
	tokio::task::spawn(async move {
		loop {
			let msg = fact_topic.next().await;
			match msg {
				Some(msg) => match msg {
					ipfs_embed::GossipEvent::Subscribed(peer) => {
						log::info!("subscribed to topic `topic/block_cid_fact`\t{}", peer);
					},
					ipfs_embed::GossipEvent::Unsubscribed(peer) => {
						log::info!("unsubscribed from topic `topic/block_cid_fact`\t{}", peer);
					},
					ipfs_embed::GossipEvent::Message(peer, msg) => {
						if let Some((block, cid)) = decode_block_cid_fact_message(msg.to_vec()) {
							{
								match get_block_cid_entry(block_cid_store_0.clone(), block) {
									Some(v) => {
										if v.self_computed && v.cid != cid {
											log::info!(
												"received CID doesn't match host computed CID"
											);
										}
										// @note what happens if have-CID is not host computed and
										// CID mismatch is encountered ?
										//
										// Need to verify/ self-compute CID and reach to a (more) stable
										// state
									},
									None => {
										match set_block_cid_entry(
											block_cid_store_0.clone(),
											block,
											BlockCidPair {
												cid,
												self_computed: false, // because this block CID is received over network !
											},
										) {
											Ok(_) => {},
											Err(e) => {
												log::info!("error: {}", e);
											},
										};
									},
								};
							}

							log::info!("received message on `topic/block_cid_fact`\t{}", peer);
						}

						// received message was not decodable !
					},
				},
				None => {
					break;
				},
			}
		}
	});

	// listen for messages received from ask topic !
	let block_cid_store_1 = block_cid_store.clone();
	// IPFS instance to be used for responding
	// back to peer when some question is asked on ask
	// channel, given that answer is known to peer !
	let ipfs_1 = ipfs.clone();
	tokio::task::spawn(async move {
		loop {
			let msg = ask_topic.next().await;
			match msg {
				Some(msg) => match msg {
					ipfs_embed::GossipEvent::Subscribed(peer) => {
						log::info!("subscribed to topic `topic/block_cid_ask`\t{}", peer);
					},
					ipfs_embed::GossipEvent::Unsubscribed(peer) => {
						log::info!("unsubscribed from topic `topic/block_cid_ask`\t{}", peer);
					},
					ipfs_embed::GossipEvent::Message(peer, msg) => {
						log::info!("received {:?} from {}", msg, peer);
						if let Some((block, cid)) = decode_block_cid_ask_message(msg.to_vec()) {
							{
								// this is a question kind message
								// on ask channel, so this peer is evaluating
								// whether it can answer it or not !
								if cid == None {
									match get_block_cid_entry(block_cid_store_1.clone(), block) {
										Some(v) => {
											// @note shall I introduce a way to denote whether CID is
											// peer computed or self computed ?
											match prepare_block_cid_ask_message(block, Some(v.cid))
											{
												Ok(msg) => {
													// respond back on same channel
													// the question is received on
													if let Ok(_) =
														ipfs_1.publish("topic/block_cid_ask", msg)
													{
														log::info!("answer question received on `topic/block_cid_ask` channel");
													} else {
														log::info!("error: failed to publish answer to question on `topic/block_cid_ask` channel");
													}
												},
												Err(msg) => {
													log::info!("error: {}", msg);
												},
											};
										},
										None => {
											// supposedly this peer can't help !
											// @note can this peer act so that it can help asking peer ?
										},
									};
								} else {
									// this is a answer kind message on ask channel
									match get_block_cid_entry(block_cid_store_1.clone(), block) {
										Some(v) => {
											if v.self_computed && v.cid != cid.unwrap() {
												log::info!(
													"received CID doesn't match host computed CID"
												);
											}
											// @note what happens if have-CID is not host computed and
											// CID mismatch is encountered ?
											//
											// Need to verify/ self-compute CID and reach to a (more) stable
											// state
										},
										None => {
											match set_block_cid_entry(
												block_cid_store_1.clone(),
												block,
												BlockCidPair {
													cid: cid.unwrap(),
													self_computed: false, // because this block CID is received over network !
												},
											) {
												Ok(_) => {},
												Err(e) => {
													log::info!("error: {}", e);
												},
											}
										},
									};
								}
							}

							log::info!("received message on `topic/block_cid_ask`\t{}", peer);
						}
					},
				},
				None => {
					break;
				},
			}
		}
	});

	// @note initialising with empty latest cid
	// but when first block is received, say number `N`
	// for preparing and pushing it to IPFS, block `N-1`'s
	// CID is required, where to get it from ?
	//
	// I need to talk to preexisting peers, who has been
	// part of network longer than I'm
	//
	// It's WIP
	let mut latest_cid: Option<Cid> = None;
	loop {
		// Receive metadata related to newly mined block
		// such as block number, dimension of data matrix etc.
		// and encode it into hierarchical structure, preparing
		// for push into ipfs.
		//
		// Finally it's pushed to ipfs, while
		// linking it with previous block data matrix's CID.
		match block_rx.recv() {
			Ok(block) => {
				match get_all_cells(&cfg.full_node_rpc, &block).await {
					Ok(cells) => {
						fn to_column_cells(col_num: u16, col: &[Option<Vec<u8>>]) -> Vec<Cell> {
							col.iter()
								.enumerate()
								.flat_map(|(i, cells)| {
									cells.clone().map(|data| Cell {
										row: i as u16,
										col: col_num,
										data: data[48..].to_vec(),
									})
								})
								.collect::<Vec<Cell>>()
						}

						// just wrapped into arc so that it's cheap
						// calling `.clone()`
						let arced_cells = Arc::new(cells);

						let columns = arced_cells
							.chunks_exact(block.max_rows as usize)
							.enumerate()
							.map(|(i, col)| to_column_cells(i as u16, col))
							.collect::<Vec<Vec<Cell>>>();

						let layout = layout_from_index(
							block.header.app_data_lookup.index.as_slice(),
							block.header.app_data_lookup.size,
						);

						let ext = reconstruct_app_extrinsics(
							layout,
							columns,
							block.max_rows as usize,
							32,
						);
						log::debug!("Reconstructed extrinsic: {:?}", ext);

						match construct_matrix(
							block.num,
							block.max_rows,
							block.max_cols,
							arced_cells,
						) {
							Ok(matrix) => {
								match push_matrix(matrix, latest_cid, &ipfs, &pin).await {
									Ok(cid) => {
										latest_cid = Some(cid);
										// publish block-cid mapping message over gossipsub network
										match prepare_block_cid_fact_message(
											block.num as i128,
											latest_cid.unwrap(), // this should be safe !
										) {
											Ok(msg) => {
												match ipfs.publish("topic/block_cid_fact", msg) {
													Ok(_) => {
														// thread-safely put an entry in local in-memory store
														// for block to cid mapping
														//
														// it can be used later for for serving clients or
														// answering to questions asked by other peers over
														// gossipsub network
														//
														// once a CID is self-computed, it'll never be rewritten even
														// when conflicting fact is found over gossipsub channel
														match set_block_cid_entry(
															block_cid_store.clone(),
															block.num as i128,
															BlockCidPair {
																cid: latest_cid.unwrap(),
																self_computed: true, // because this block CID is self-computed !
															},
														) {
															Ok(_) => {},
															Err(e) => {
																log::info!("error: {}", e);
															},
														};
														log::info!(
															"✅ Block {} available\t{}",
															block.num,
															latest_cid.unwrap().clone()
														);
													},
													Err(_) => {
														log::info!("error: failed to publish fact on `topic/block_cid_fact` topic");
													},
												};
											},
											Err(msg) => {
												log::info!("error: {}", msg);
											},
										};
									},
									Err(msg) => {
										log::info!("error: {}", msg);
									},
								}
							},
							Err(msg) => {
								log::info!("error: {}", msg);
							},
						};
					},
					Err(e) => {
						log::info!("error: {}", e);
					},
				};
			},
			Err(e) => {
				log::info!("Error encountered while listening for blocks: {}", e);
				break;
			},
		};
	}

	destroy_rx.recv()?; // waiting for signal to kill self !
	Ok(())
}

// Given a certain block number, this function prepares a question-kind message
// and sends over gossip network on ask channel ( topic )
//
// After that it wait for some time ( to be more specific as of now 4 seconds )
//
// Then acquires local data store ( block to cid mapping ) lock
// and looks up whether some helpful peer has already answered
// question or not
//
// If yes returns obtained CID
//
// @note How to verify whether this CID is correct or not ?
async fn ask_block_cid(
	block: i128,
	ipfs: &Ipfs<IPFSDefaultParams>,
	store: Arc<Mutex<HashMap<i128, BlockCidPair>>>,
) -> Result<Cid, String> {
	// send message over gossip network !
	match prepare_block_cid_ask_message(block, None) {
		Ok(msg) => {
			if let Err(_) = ipfs.publish("topic/block_cid_ask", msg) {
				return Err("failed to send gossip message".to_owned());
			}
			// wait for 4 seconds !
			std::thread::sleep(Duration::from_secs(4));
			// thread-safely attempt to read from data store
			// whether question has been answer by some peer already
			// or not
			{
				let handle = store.lock().unwrap();
				match handle.get(&block) {
					Some(v) => Ok(v.cid),
					None => Err("failed to find CID".to_owned()),
				}
			}
		},
		Err(msg) => Err(msg),
	}
}

pub async fn make_client(
	seed: u64,
	port: u16,
	path: &str,
) -> anyhow::Result<Ipfs<IPFSDefaultParams>> {
	let sweep_interval = Duration::from_secs(60);

	let path_buf = std::path::PathBuf::from_str(path).unwrap();
	let storage = StorageConfig::new(None, 10, sweep_interval);
	let mut network = NetworkConfig::new(path_buf, keypair(seed));
	network.mdns = None;

	let ipfs = Ipfs::<IPFSDefaultParams>::new(ipfs_embed::Config { storage, network }).await?;
	let mut events = ipfs.swarm_events();

	let mut stream = ipfs.listen_on(format!("/ip4/127.0.0.1/tcp/{}", port).parse()?)?;
	if let ipfs_embed::ListenerEvent::NewListenAddr(_) = stream.next().await.unwrap() {
		/* do nothing useful here, just ensure ipfs-node has
		started listening on specified port */
	}

	tokio::task::spawn(async move {
		while let Some(event) = events.next().await {
			let event = match event {
				ipfs_embed::Event::NewListener(_) => Some(Event::NewListener),
				ipfs_embed::Event::NewListenAddr(_, addr) => Some(Event::NewListenAddr(addr)),
				ipfs_embed::Event::ExpiredListenAddr(_, addr) => {
					Some(Event::ExpiredListenAddr(addr))
				},
				ipfs_embed::Event::ListenerClosed(_) => Some(Event::ListenerClosed),
				ipfs_embed::Event::NewExternalAddr(addr) => Some(Event::NewExternalAddr(addr)),
				ipfs_embed::Event::ExpiredExternalAddr(addr) => {
					Some(Event::ExpiredExternalAddr(addr))
				},
				ipfs_embed::Event::Discovered(peer_id) => Some(Event::Discovered(peer_id)),
				ipfs_embed::Event::Unreachable(peer_id) => Some(Event::Unreachable(peer_id)),
				ipfs_embed::Event::Connected(peer_id) => Some(Event::Connected(peer_id)),
				ipfs_embed::Event::Disconnected(peer_id) => Some(Event::Disconnected(peer_id)),
				ipfs_embed::Event::Subscribed(peer_id, topic) => {
					Some(Event::Subscribed(peer_id, topic))
				},
				ipfs_embed::Event::Unsubscribed(peer_id, topic) => {
					Some(Event::Unsubscribed(peer_id, topic))
				},
				ipfs_embed::Event::Bootstrapped => Some(Event::Bootstrapped),
				ipfs_embed::Event::NewHead(head) => Some(Event::NewHead(*head.id(), head.len())),
			};
			if let Some(_event) = event {
				#[cfg(feature = "logs")]
				log::info!("{}", _event);
			}
		}
	});

	Ok(ipfs)
}

pub fn keypair(i: u64) -> Keypair {
	let mut keypair = [0; 32];
	keypair[..8].copy_from_slice(&i.to_be_bytes());
	let secret = SecretKey::from_bytes(&keypair).unwrap();
	let public = PublicKey::from(&secret);
	Keypair { secret, public }
}

pub fn peer_id(i: u64) -> PeerId { keypair(i).to_peer_id() }

// Following two are utility functions for interacting with local on-disk data store
// where block -> cid mapping is maintained

pub fn get_block_cid_entry(
	store: Arc<DBWithThreadMode<SingleThreaded>>,
	block: i128,
) -> Option<crate::types::BlockCidPair> {
	match store.get_cf(
		store.cf_handle(crate::consts::BLOCK_CID_CF).unwrap(),
		block.to_be_bytes(),
	) {
		Ok(v) => match v {
			Some(v) => {
				let pair: crate::types::BlockCidPersistablePair =
					serde_json::from_slice(&v).unwrap();
				Some(crate::types::BlockCidPair {
					cid: Cid::try_from(pair.cid).unwrap(),
					self_computed: pair.self_computed,
				})
			},
			None => None,
		},
		Err(_) => None,
	}
}

fn layout_from_index(index: &[(u32, u32)], size: u32) -> Vec<(u32, u32)> {
	let mut layout: Vec<(u32, u32)> = Vec::with_capacity(index.len() + 1);
	if index.is_empty() {
		return vec![(0, size)];
	}
	layout.push((0, index[0].1 - 1));
	for i in 1..index.len() - 1 {
		layout.push((index[i - 1].0, index[i].1 - index[i - 1].1));
	}
	if index.len() > 1 {
		let last_idx = index.len() - 1;
		layout.push((
			index[last_idx - 1].0,
			index[last_idx].1 - index[last_idx - 1].1,
		));
		layout.push((index[last_idx].0, size - index[last_idx].1 + 1));
	}
	layout
}
#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_layout_from_index() {
		let expected = vec![(0, 2), (1, 3), (2, 5)];
		assert_eq!(layout_from_index(&[(1, 3), (2, 6)], 10), expected);

		let expected = vec![(0, 11), (1, 3), (3, 6)];
		assert_eq!(layout_from_index(&[(1, 12), (3, 15)], 20), expected);
	}
}

pub fn set_block_cid_entry(
	store: Arc<DBWithThreadMode<SingleThreaded>>,
	block: i128,
	pair: BlockCidPair,
) -> Result<(), String> {
	let serialisable_pair = crate::types::BlockCidPersistablePair {
		cid: pair.cid.to_string(),
		self_computed: pair.self_computed,
	};
	let serialised = serde_json::to_string(&serialisable_pair).unwrap();

	match store.put_cf(
		store.cf_handle(crate::consts::BLOCK_CID_CF).unwrap(),
		block.to_be_bytes(),
		serialised.as_bytes(),
	) {
		Ok(_) => Ok(()),
		Err(_) => Err("failed to put block -> cid entry in database".to_owned()),
	}
}
