use std::{
	collections::{BTreeMap, HashMap},
	iter::repeat,
	str::FromStr,
	sync::{
		mpsc::{Receiver, SyncSender},
		Arc, Mutex,
	},
	time::Duration,
};

use anyhow::{Context, Result};
use async_std::stream::StreamExt;
use futures::stream;
use ipfs_embed::{
	Cid, DefaultParams as IPFSDefaultParams, GossipEvent, Ipfs, Keypair, Multiaddr, NetworkConfig,
	PeerId, PublicKey, SecretKey, StorageConfig, TempPin, ToLibp2p,
};
use kate_recovery::com::{reconstruct_app_extrinsics, Cell};
use libipld::Ipld;
use rocksdb::DB;
use tokio::time::sleep;

use crate::{
	consts::BLOCK_CID_CF,
	data::{
		construct_matrix, decode_block_cid_ask_message, decode_block_cid_fact_message, empty_cells,
		extract_block, extract_cell, extract_links, get_matrix, matrix_cells, non_empty_cells_len,
		prepare_block_cid_ask_message, prepare_block_cid_fact_message, push_matrix, Matrix,
	},
	ensure,
	error::{self, into_warning, IpldField, StoreType, Topic},
	rpc::{check_http, get_cells},
	types::{
		BlockCidPair, BlockCidPersistablePair, CellContentQueryPayload, Cells, ClientMsg, Event,
	},
};

#[tokio::main]
pub async fn run_client(
	cfg: super::types::RuntimeConfig,
	store: Arc<DB>,
	block_rx: Receiver<ClientMsg>,
	self_info_tx: SyncSender<(PeerId, Multiaddr)>,
	destroy_rx: Receiver<bool>,
	cell_query_rx: Receiver<CellContentQueryPayload>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
	let ipfs = make_client(cfg.ipfs_seed, cfg.ipfs_port, &cfg.ipfs_path).await?;
	let pin = ipfs.create_temp_pin()?;

	// inform invoker about self
	self_info_tx.send((ipfs.local_peer_id(), ipfs.listeners()[0].clone()))?;

	// bootstrap client with non-empty set of
	// application clients
	if !cfg.bootstraps.is_empty() {
		ipfs.bootstrap(
			&cfg.bootstraps
				.into_iter()
				.map(|(a, b)| (PeerId::from_str(&a).unwrap(), b))
				.collect::<Vec<(_, _)>>()[..],
		)
		.await?;
	}

	// Process Cell Queries.
	let cell_ipfs = ipfs.clone();
	let cell_store = Arc::clone(&store);
	tokio::task::spawn(async move {
		cell_query_rx.into_iter().for_each(|query| {
			let _ = do_cell_query(&cell_ipfs, &cell_store, query).inspect_err(into_warning);
		});
	});

	// block to CID mapping to locally kept here ( in thead safe manner ! )

	// broadcast fact i.e. some block number is mapped to some
	// Cid ( of respective block data matrix ) to all peers subscribed
	let fact_topic = ipfs.subscribe("topic/block_cid_fact").unwrap();

	// listen for messages received on fact topic !
	let fact_ipfs = ipfs.clone();
	let fact_store = Arc::clone(&store);
	tokio::task::spawn(async move {
		fact_topic.for_each(|msg| {
			const TOPIC: &str = "topic/block_cid_fact";
			let store = fact_store.as_ref();
			do_gossip_msg(&fact_ipfs, store, msg, TOPIC, do_gossip_fact_msg)
		});
	});

	// ask over gossip network which Cid is associated with which block number
	// and expect some one, who knows, will answer it
	let ask_topic = ipfs.subscribe("topic/block_cid_ask").unwrap();
	// IPFS instance to be used for responding
	// back to peer when some question is asked on ask
	// channel, given that answer is known to peer !
	let ask_ipfs = ipfs.clone();
	let ask_store = Arc::clone(&store);
	tokio::task::spawn(async move {
		ask_topic
			.zip(stream::iter(repeat(ask_store)))
			.for_each(|(msg, store)| {
				const TOPIC: &str = "topic/block_cid_ask";
				do_gossip_msg(&ask_ipfs, &store, msg, TOPIC, do_gossip_ask_msg)
			});
	});

	let rpc = cfg.full_node_rpc.clone();
	let _ = process_received_blocks(&ipfs, store.as_ref(), &pin, block_rx, rpc).await?;

	destroy_rx.recv()?; // waiting for signal to kill self !
	Ok(())
}

fn do_cell_query(
	ipfs: &Ipfs<IPFSDefaultParams>,
	store: &DB,
	query: CellContentQueryPayload,
) -> Result<(), error::App> {
	let cid = get_block_cid_entry(store, query.block as i128)
		.ok_or_else(|| {
			let _ = query.res_chan.try_send(None);
			error::Client::BlockCIDNotFound(query.block)
		})?
		.cid;

	let have = ipfs
		.contains(&cid)
		.with_context(|| format!("IPFS does not contain CID {}", cid))?;
	ensure!(have, error::Client::CIDNotFound(cid, StoreType::Ipfs));

	let dec_mat: Ipld = ipfs
		.get(&cid)?
		.ipld()
		.with_context(|| format!("Empty Ipld on  CID {}", cid))?;

	if let Ipld::StringMap(map) = dec_mat {
		do_ipld_cell_query(ipfs, query, map)?;
	}

	Ok(())
}

fn do_ipld_cell_query(
	ipfs: &Ipfs<IPFSDefaultParams>,
	query: CellContentQueryPayload,
	map: BTreeMap<String, Ipld>,
) -> Result<(), error::App> {
	let block = map
		.get("block")
		.and_then(extract_block)
		.ok_or(error::Client::MissingFieldOnIpld(IpldField::Block))?;
	ensure!(
		block == query.block as i128,
		error::Client::MismatchBlockOnIpld(block, query.block)
	);

	let cids = map
		.get("columns")
		.and_then(extract_links)
		.ok_or(error::Client::MissingFieldOnIpld(IpldField::Columns))?;

	ensure!(
		(query.col as usize) < cids.len(),
		error::Client::MaxColumnExceeded(query.col, cids.len())
	);
	let col_cid = cids[query.col as usize].ok_or(error::Client::EmptyColumnOnCidList(query.col))?;

	let dec_col = ipfs
		.get(&col_cid)
		.with_context(|| format!("IPFS cannot fetch the column CID {}", col_cid))?
		.ipld()
		.with_context(|| format!("Empty Ipld on column CID {}", col_cid))?;

	let row_cids =
		extract_links(&dec_col).ok_or(error::Client::MissingFieldOnIpld(IpldField::Rows))?;
	ensure!(
		(query.row as usize) < row_cids.len(),
		error::Client::MaxRowExceeded(query.row, row_cids.len())
	);

	let row_cid =
		row_cids[query.row as usize].ok_or(error::Client::EmptyRowOnCidList(query.row))?;

	let dec_cell = ipfs
		.get(&row_cid)
		.with_context(|| format!("IPFS cannot fetch the row CID {}", row_cid))?
		.ipld()
		.with_context(|| "Failed to IPLD decode data matrix cell".to_string())?;

	// respond back with content of cell
	query
		.res_chan
		.try_send(extract_cell(&dec_cell))
		.map_err(|err| error::Client::ResponseChannelFailed(err).into())
}

fn do_gossip_msg<F>(
	ipfs: &Ipfs<IPFSDefaultParams>,
	store: &DB,
	msg: GossipEvent,
	topic: &str,
	msg_handler: F,
) where
	F: FnOnce(&Ipfs<IPFSDefaultParams>, &DB, PeerId, Vec<u8>) -> Result<(), error::App>,
{
	match msg {
		GossipEvent::Subscribed(peer) => {
			log::info!("subscribed to topic `{}`\t{}", topic, peer);
		},
		GossipEvent::Unsubscribed(peer) => {
			log::info!("unsubscribed from topic `{}`\t{}", topic, peer);
		},
		GossipEvent::Message(peer, msg) => {
			log::info!("received message on `{}`\t{}", topic, peer);
			let _ = msg_handler(ipfs, store, peer, msg.to_vec()).inspect_err(into_warning);
		},
	}
}

fn do_gossip_fact_msg(
	_ipfs: &Ipfs<IPFSDefaultParams>,
	store: &DB,
	peer: PeerId,
	msg: Vec<u8>,
) -> Result<(), error::App> {
	if let Some((block, cid)) = decode_block_cid_fact_message(msg) {
		log::info!("received message on `topic/block_cid_fact`\t{}", peer);

		match get_block_cid_entry(store, block) {
			Some(v) => {
				// @note what happens if have-CID is not host computed and
				// CID mismatch is encountered ?
				//
				// Need to verify/ self-compute CID and reach to a (more) stable
				// state
				ensure!(
					!v.self_computed && v.cid == cid,
					error::Client::ReceivedCIDMismatch(v.cid, cid)
				);
			},
			None => {
				let _ = set_block_cid_entry(store, block, BlockCidPair {
					cid,
					self_computed: false, // because this block CID is received over network !
				})?;
			},
		};
	}
	Ok(())
}

fn do_gossip_ask_msg(
	ipfs: &Ipfs<IPFSDefaultParams>,
	store: &DB,
	_peer: PeerId,
	msg: Vec<u8>,
) -> Result<(), error::App> {
	match decode_block_cid_ask_message(msg) {
		Some((block, None)) => {
			// this is a question kind message
			// on ask channel, so this peer is evaluating
			// whether it can answer it or not !
			if let Some(v) = get_block_cid_entry(store, block) {
				// @note shall I introduce a way to denote whether CID is
				// peer computed or self computed ?
				let ask_msg = prepare_block_cid_ask_message(block, Some(v.cid));

				// respond back on same channel
				// the question is received on
				ipfs.publish("topic/block_cid_ask", ask_msg)?;
				log::info!("answer question received on `topic/block_cid_ask` channel");
			}
		},
		Some((block, Some(cid))) => {
			// this is a answer kind message on ask channel
			match get_block_cid_entry(store, block) {
				Some(v) => {
					ensure!(
						!v.self_computed && v.cid == cid,
						error::Client::ReceivedCIDMismatch(v.cid, cid)
					);
					// @note what happens if have-CID is not host computed and
					// CID mismatch is encountered ?
					//
					// Need to verify/ self-compute CID and reach to a (more) stable
					// state
				},
				None => {
					let pair = BlockCidPair {
						cid,
						self_computed: false,
					};
					set_block_cid_entry(store, block, pair)?;
				},
			}
		},
		_ => {},
	}
	Ok(())
}

async fn process_received_blocks(
	ipfs: &Ipfs<IPFSDefaultParams>,
	store: &DB,
	pin: &TempPin,
	block_rx: Receiver<ClientMsg>,
	full_node_rpc: Vec<String>,
) -> Result<(), error::App> {
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
	for block in block_rx {
		// Receive metadata related to newly mined block
		// such as block number, dimension of data matrix etc.
		// and encode it into hierarchical structure, preparing
		// for push into ipfs.
		//
		// Finally it's pushed to ipfs, while
		// linking it with previous block data matrix's CID.
		let rpc_ = check_http(&full_node_rpc).await?;
		let block_cid_entry = get_block_cid_entry(store, block.num as i128).map(|pair| pair.cid);

		let ipfs_cells = get_matrix(ipfs, block_cid_entry)
			.inspect_err(|err| log::error!("Fail to fetch cells from IPFS: {}", err))
			.unwrap_or_default();

		let requested_cells = empty_cells(&ipfs_cells, block.max_cols, block.max_rows);

		log::info!(
			"Got {} cells from IPFS, requesting {} from full node",
			non_empty_cells_len(&ipfs_cells),
			requested_cells.len()
		);

		let cells = get_cells(&rpc_, &block, &requested_cells).await;
		latest_cid = process_cells(ipfs, store, pin, &block, &ipfs_cells, latest_cid, cells)
			.await
			.inspect_err(into_warning)
			.ok()
			.or(latest_cid);
	}

	Ok(())
}

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

async fn process_cells(
	ipfs: &Ipfs<IPFSDefaultParams>,
	block_cid_store: &DB,
	pin: &TempPin,
	block: &ClientMsg,
	ipfs_cells: &Matrix,
	latest_cid: Option<Cid>,
	mut cells: Cells,
) -> Result<Cid, error::App> {
	for (row, col) in matrix_cells(block.max_rows, block.max_cols) {
		let index = col * block.max_rows as usize + row;

		if cells[index].is_none() {
			cells[index] = ipfs_cells
				.get(col)
				.and_then(|col| col.get(row))
				.and_then(|val| val.to_owned());
		}
	}

	// just wrapped into arc so that it's cheap
	// calling `.clone()`
	let columns = cells
		.chunks_exact(block.max_rows as usize)
		.enumerate()
		.map(|(i, col)| to_column_cells(i as u16, col))
		.collect::<Vec<Vec<Cell>>>();

	let layout = layout_from_index(
		block.header.app_data_lookup.index.as_slice(),
		block.header.app_data_lookup.size,
	);

	let ext = reconstruct_app_extrinsics(layout, columns, (block.max_rows / 2) as usize, 32);
	log::debug!("Reconstructed extrinsic: {:?}", ext);

	let matrix = construct_matrix(block.num, block.max_rows, block.max_cols, &cells)?;
	let cid = push_matrix(matrix, latest_cid, ipfs, pin).await?;

	// publish block-cid mapping message over gossipsub network
	let msg = prepare_block_cid_fact_message(block.num as i128, cid);
	let _ = ipfs.publish("topic/block_cid_fact", msg)?;

	// thread-safely put an entry in local in-memory store
	// for block to cid mapping
	//
	// it can be used later for for serving clients or
	// answering to questions asked by other peers over
	// gossipsub network
	//
	// once a CID is self-computed, it'll never be rewritten even
	// when conflicting fact is found over gossipsub channel
	let _ = set_block_cid_entry(
		block_cid_store,
		block.num as i128,
		// because this block CID is self-computed !
		BlockCidPair {
			cid,
			self_computed: true,
		},
	)?;

	log::info!("✅ Block {} available\t{}", block.num, cid);
	Ok(cid)
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
#[allow(dead_code)]
async fn ask_block_cid(
	block: i128,
	ipfs: &Ipfs<IPFSDefaultParams>,
	store: Arc<Mutex<HashMap<i128, BlockCidPair>>>,
) -> Result<Cid, error::App> {
	// send message over gossip network !
	let msg = prepare_block_cid_ask_message(block, None);

	ipfs.publish("topic/block_cid_ask", msg)
		.map_err(|_| error::IpfsClient::SendGossipMessage(Topic::Ask))?;

	// wait for 4 seconds !
	sleep(Duration::from_secs(4)).await;

	// thread-safely attempt to read from data store
	// whether question has been answer by some peer already
	// or not
	let cid = store
		.lock()
		.unwrap()
		.get(&block)
		.ok_or(error::Client::BlockNotFound(block))?
		.cid;

	Ok(cid)
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

#[allow(dead_code)]
pub fn peer_id(i: u64) -> PeerId { keypair(i).to_peer_id() }

// Following two are utility functions for interacting with local on-disk data store
// where block -> cid mapping is maintained

pub fn get_block_cid_entry(store: &DB, block: i128) -> Option<BlockCidPair> {
	// Load from storage.
	let column = store.cf_handle(BLOCK_CID_CF)?;
	let key = block.to_be_bytes();
	let encoded_pair = store.get_cf(&column, key).ok().flatten()?;

	// Transform into `BlockCidPair`.
	let pair: BlockCidPersistablePair = serde_json::from_slice(&encoded_pair).ok()?;
	let cid = pair.cid.try_into().ok()?;
	let block_cid_pair = BlockCidPair {
		cid,
		self_computed: pair.self_computed,
	};

	Some(block_cid_pair)
}

fn layout_from_index(index: &[(u32, u32)], size: u32) -> Vec<(u32, u32)> {
	if index.is_empty() {
		return vec![(0, size)];
	}

	let (app_ids, offsets): (Vec<_>, Vec<_>) = index.iter().cloned().unzip();
	// Prepend app_id zero
	let mut app_ids_ext = vec![0];
	app_ids_ext.extend(app_ids);

	// Prepend offset 0 for app_id 0
	let mut offsets_ext = vec![0];
	offsets_ext.extend(offsets);

	let mut sizes = offsets_ext[0..offsets_ext.len() - 1]
		.iter()
		.zip(offsets_ext[1..].iter())
		.map(|(a, b)| b - a)
		.collect::<Vec<_>>();

	let remaining_size: u32 = size - sizes.iter().sum::<u32>();
	sizes.push(remaining_size);

	app_ids_ext.into_iter().zip(sizes.into_iter()).collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_layout_from_index() {
		let expected = vec![(0, 3), (1, 3), (2, 4)];
		assert_eq!(layout_from_index(&[(1, 3), (2, 6)], 10), expected);

		let expected = vec![(0, 12), (1, 3), (3, 5)];
		assert_eq!(layout_from_index(&[(1, 12), (3, 15)], 20), expected);

		let expected = vec![(0, 1), (1, 5)];
		assert_eq!(layout_from_index(&[(1, 1)], 6), expected);
	}
}

pub fn set_block_cid_entry(store: &DB, block: i128, pair: BlockCidPair) -> Result<(), error::App> {
	let serialisable_pair = BlockCidPersistablePair {
		cid: pair.cid.to_string(),
		self_computed: pair.self_computed,
	};
	let serialised = serde_json::to_string(&serialisable_pair)
		.expect("`BlockCidPersistablePair` is unfallible on serde serialization .qed");
	let column = store
		.cf_handle(BLOCK_CID_CF)
		.expect("`BLOCK_CID_CF` is valid .qed");

	store
		.put_cf(&column, block.to_be_bytes(), serialised.as_bytes())
		.map_err(error::App::from)
}
