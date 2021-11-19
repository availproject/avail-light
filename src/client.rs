extern crate anyhow;
extern crate async_std;
extern crate ed25519_dalek;
extern crate ipfs_embed;
extern crate libipld;
extern crate rand;
extern crate tempdir;
extern crate tokio;

use crate::types::Event;
use async_std::stream::StreamExt;
use ed25519_dalek::{PublicKey, SecretKey};
use ipfs_embed::{DefaultParams as IPFSDefaultParams, Ipfs, Keypair, NetworkConfig, StorageConfig};
use std::time::Duration;
use tempdir::TempDir;

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
        println!(
            "self: {}\t{:?}",
            ipfs.local_peer_id(),
            ipfs.listeners().clone()
        );
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
