extern crate anyhow;
extern crate async_std;
extern crate ed25519_dalek;
extern crate ipfs_embed;
extern crate libipld;
extern crate rand;
extern crate tempdir;
extern crate tokio;

use crate::data::{construct_matrix, push_matrix};
use crate::types::{ClientMsg, Event};
use async_std::stream::StreamExt;
use ed25519_dalek::{PublicKey, SecretKey};
use ipfs_embed::{
    Cid, DefaultParams as IPFSDefaultParams, Ipfs, Keypair, Multiaddr, NetworkConfig, PeerId,
    StorageConfig,
};
use std::sync::mpsc::{Receiver, SyncSender};
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

                println!(
                    "✅ Block {} available on IPFS\t{}",
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
