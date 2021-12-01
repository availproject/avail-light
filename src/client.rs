extern crate anyhow;
extern crate async_std;
extern crate ed25519_dalek;
extern crate ipfs_embed;
extern crate libipld;
extern crate rand;
extern crate tempdir;
extern crate tokio;

use crate::data::{
    construct_matrix, decode_block_cid_ask_message, decode_block_cid_fact_message,
    prepare_block_cid_ask_message, prepare_block_cid_fact_message, push_matrix,
};
use crate::types::{BlockCidPair, ClientMsg, Event};
use async_std::stream::StreamExt;
use ed25519_dalek::{PublicKey, SecretKey};
use ipfs_embed::{
    Cid, DefaultParams as IPFSDefaultParams, Ipfs, Keypair, Multiaddr, NetworkConfig, PeerId,
    StorageConfig,
};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempdir::TempDir;

#[tokio::main]
pub async fn run_client(
    seed: u64,
    peers: Vec<(PeerId, Multiaddr)>,
    block_rx: Receiver<ClientMsg>,
    self_info_tx: SyncSender<(PeerId, Multiaddr)>,
    destroy_rx: Receiver<bool>,
) -> anyhow::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // @note make ipfs node's port user choosable
    let ipfs = make_client(seed, 37000, &format!("node_{}", seed)).await?;
    let pin = ipfs.create_temp_pin()?;

    // inform invoker about self
    self_info_tx.send((ipfs.local_peer_id(), ipfs.listeners()[0].clone()))?;

    // bootstrap client with non-empty set of
    // application clients
    if peers.len() > 0 {
        ipfs.bootstrap(&peers).await?;
    }

    // block to CID mapping to locally kept here ( in thead safe manner ! )
    let block_cid_store = Arc::new(Mutex::new(HashMap::<i128, BlockCidPair>::new()));

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
                        println!("subscribed to topic `topic/block_cid_fact`\t{}", peer);
                    }
                    ipfs_embed::GossipEvent::Unsubscribed(peer) => {
                        println!("unsubscribed from topic `topic/block_cid_fact`\t{}", peer);
                    }
                    ipfs_embed::GossipEvent::Message(peer, msg) => {
                        if let Some((block, cid)) = decode_block_cid_fact_message(msg.to_vec()) {
                            {
                                let mut handle = block_cid_store_0.lock().unwrap();

                                match handle.get(&block) {
                                    Some(v) => {
                                        if v.self_computed && v.cid != cid {
                                            println!(
                                                "received CID doesn't match host computed CID"
                                            );
                                        }
                                        // @note what happens if have-CID is not host computed and
                                        // CID mismatch is encountered ?
                                        //
                                        // Need to verify/ self-compute CID and reach to a (more) stable
                                        // state
                                    }
                                    None => {
                                        handle.insert(
                                            block,
                                            BlockCidPair {
                                                cid: cid,
                                                self_computed: false, // because this block CID is received over network !
                                            },
                                        );
                                    }
                                };
                            }

                            println!("received message on `topic/block_cid_fact`\t{}", peer);
                        }

                        // received message was not decodable !
                    }
                },
                None => {
                    break;
                }
            }
        }
    });

    // listen for messages received from ask topic !
    let block_cid_store_1 = block_cid_store.clone();
    // IPFS instance to be used for responding
    // back to peer when some question is asked on ask
    // channel, given that answer is known to peer !
    let ipfs_clone = ipfs.clone();
    tokio::task::spawn(async move {
        loop {
            let msg = ask_topic.next().await;
            match msg {
                Some(msg) => match msg {
                    ipfs_embed::GossipEvent::Subscribed(peer) => {
                        println!("subscribed to topic `topic/block_cid_ask`\t{}", peer);
                    }
                    ipfs_embed::GossipEvent::Unsubscribed(peer) => {
                        println!("unsubscribed from topic `topic/block_cid_ask`\t{}", peer);
                    }
                    ipfs_embed::GossipEvent::Message(peer, msg) => {
                        println!("received {:?} from {}", msg, peer);
                        if let Some((block, cid)) = decode_block_cid_ask_message(msg.to_vec()) {
                            {
                                let mut handle = block_cid_store_1.lock().unwrap();

                                // this is a question kind message
                                // on ask channel, so this peer is evaluating
                                // whether it can answer it or not !
                                if cid == None {
                                    match handle.get(&block) {
                                        Some(v) => {
                                            // @note shall I introduce a way to denote whether CID is
                                            // peer computed or self computed ?
                                            let msg =
                                                prepare_block_cid_ask_message(block, Some(v.cid));
                                            ipfs_clone
                                                .publish("topic/block_cid_ask", msg) // respond back on same channel
                                                .unwrap(); // the question is received on
                                        }
                                        None => {
                                            // supposedly this peer can't help !
                                            // @note can this peer act so that it can help asking peer ?
                                        }
                                    };
                                } else {
                                    // this is a answer kind message on ask channel
                                    match handle.get(&block) {
                                        Some(v) => {
                                            if v.self_computed && v.cid != cid.unwrap() {
                                                println!(
                                                    "received CID doesn't match host computed CID"
                                                );
                                            }
                                            // @note what happens if have-CID is not host computed and
                                            // CID mismatch is encountered ?
                                            //
                                            // Need to verify/ self-compute CID and reach to a (more) stable
                                            // state
                                        }
                                        None => {
                                            handle.insert(
                                                block,
                                                BlockCidPair {
                                                    cid: cid.unwrap(),
                                                    self_computed: false, // because this block CID is received over network !
                                                },
                                            );
                                        }
                                    };
                                }
                            }

                            println!("received message on `topic/block_cid_ask`\t{}", peer);
                        }
                    }
                },
                None => {
                    break;
                }
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
                let matrix = construct_matrix(block.num, block.max_rows, block.max_cols).await;
                latest_cid = Some(push_matrix(matrix, latest_cid.clone(), &ipfs, &pin).await?);

                // publish block-cid mapping message over gossipsub network
                let msg = prepare_block_cid_fact_message(block.num as i128, latest_cid.unwrap());
                ipfs.publish("topic/block_cid_fact", msg).unwrap();

                // thread-safely put an entry in local in-memory store
                // for block to cid mapping
                //
                // it can be used later for for serving clients or
                // answering to questions asked by other peers over
                // gossipsub network
                //
                // once a CID is self-computed, it'll never be rewritten even
                // when conflicting fact is found over gossipsub channel
                {
                    let mut handle = block_cid_store.lock().unwrap();
                    handle.insert(
                        block.num as i128,
                        BlockCidPair {
                            cid: latest_cid.unwrap(),
                            self_computed: true, // because this block CID is self-computed !
                        },
                    );
                }

                println!(
                    "✅ Block {} available\t{}",
                    block.num,
                    latest_cid.unwrap().clone()
                );
            }
            Err(e) => {
                println!("Error encountered while listening for blocks: {}", e);
                break;
            }
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
) -> Result<Cid, &str> {
    // send message over gossip network !
    let msg = prepare_block_cid_ask_message(block, None);
    if let Err(_) = ipfs.publish("topic/block_cid_ask", msg) {
        return Err("failed to send gossip message");
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
            None => Err("failed to find CID"),
        }
    }
}

pub async fn make_client(
    seed: u64,
    port: u16,
    path: &str,
) -> anyhow::Result<Ipfs<IPFSDefaultParams>> {
    let sweep_interval = Duration::from_secs(60);

    let path_buf = TempDir::new(path)?;
    let storage = StorageConfig::new(None, 10, sweep_interval);
    let mut network = NetworkConfig::new(path_buf.path().into(), keypair(seed));
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
                }
                ipfs_embed::Event::ListenerClosed(_) => Some(Event::ListenerClosed),
                ipfs_embed::Event::NewExternalAddr(addr) => Some(Event::NewExternalAddr(addr)),
                ipfs_embed::Event::ExpiredExternalAddr(addr) => {
                    Some(Event::ExpiredExternalAddr(addr))
                }
                ipfs_embed::Event::Discovered(peer_id) => Some(Event::Discovered(peer_id)),
                ipfs_embed::Event::Unreachable(peer_id) => Some(Event::Unreachable(peer_id)),
                ipfs_embed::Event::Connected(peer_id) => Some(Event::Connected(peer_id)),
                ipfs_embed::Event::Disconnected(peer_id) => Some(Event::Disconnected(peer_id)),
                ipfs_embed::Event::Subscribed(peer_id, topic) => {
                    Some(Event::Subscribed(peer_id, topic))
                }
                ipfs_embed::Event::Unsubscribed(peer_id, topic) => {
                    Some(Event::Unsubscribed(peer_id, topic))
                }
                ipfs_embed::Event::Bootstrapped => Some(Event::Bootstrapped),
                ipfs_embed::Event::NewHead(head) => Some(Event::NewHead(*head.id(), head.len())),
            };
            if let Some(_event) = event {
                #[cfg(feature = "logs")]
                println!("{}", _event);
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
