extern crate anyhow;
extern crate async_std;
extern crate ed25519_dalek;
extern crate ipfs_embed;
extern crate libipld;
extern crate rand;
extern crate tempdir;
extern crate tokio;

use async_std::stream::StreamExt;
use ed25519_dalek::{PublicKey, SecretKey};
use ipfs_embed::{
    Block, DefaultParams as IPFSDefaultParams, Ipfs, Keypair, Multiaddr, NetworkConfig, PeerId,
    StorageConfig, StreamId,
};
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

#[derive(Debug, Eq, PartialEq)]
pub enum Event {
    NewListener,
    NewListenAddr(Multiaddr),
    ExpiredListenAddr(Multiaddr),
    ListenerClosed,
    NewExternalAddr(Multiaddr),
    ExpiredExternalAddr(Multiaddr),
    Discovered(PeerId),
    Unreachable(PeerId),
    Connected(PeerId),
    Disconnected(PeerId),
    Subscribed(PeerId, String),
    Unsubscribed(PeerId, String),
    Block(Block<IPFSDefaultParams>),
    Flushed,
    Synced,
    Bootstrapped,
    NewHead(StreamId, u64),
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::NewListener => write!(f, "<new-listener")?,
            Self::NewListenAddr(addr) => write!(f, "<new-listen-addr {}", addr)?,
            Self::ExpiredListenAddr(addr) => write!(f, "<expired-listen-addr {}", addr)?,
            Self::ListenerClosed => write!(f, "<listener-closed")?,
            Self::NewExternalAddr(addr) => write!(f, "<new-external-addr {}", addr)?,
            Self::ExpiredExternalAddr(addr) => write!(f, "<expired-external-addr {}", addr)?,
            Self::Discovered(peer) => write!(f, "<discovered {}", peer)?,
            Self::Unreachable(peer) => write!(f, "<unreachable {}", peer)?,
            Self::Connected(peer) => write!(f, "<connected {}", peer)?,
            Self::Disconnected(peer) => write!(f, "<disconnected {}", peer)?,
            Self::Subscribed(peer, topic) => write!(f, "<subscribed {} {}", peer, topic)?,
            Self::Unsubscribed(peer, topic) => write!(f, "<unsubscribed {} {}", peer, topic)?,
            Self::Block(block) => {
                write!(f, "<block {} ", block.cid())?;
                for byte in block.data() {
                    write!(f, "{:02x}", byte)?;
                }
            }
            Self::Flushed => write!(f, "<flushed")?,
            Self::Synced => write!(f, "<synced")?,
            Self::Bootstrapped => write!(f, "<bootstrapped")?,
            Self::NewHead(id, offset) => write!(f, "<newhead {} {}", id, offset)?,
        }
        Ok(())
    }
}

impl std::str::FromStr for Event {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        let mut parts = s.split_whitespace();
        Ok(match parts.next() {
            Some("<new-listener") => Self::NewListener,
            Some("<new-listen-addr") => {
                let addr = parts.next().unwrap().parse()?;
                Self::NewListenAddr(addr)
            }
            Some("<expired-listen-addr") => {
                let addr = parts.next().unwrap().parse()?;
                Self::ExpiredListenAddr(addr)
            }
            Some("<listener-closed") => Self::ListenerClosed,
            Some("<new-external-addr") => {
                let addr = parts.next().unwrap().parse()?;
                Self::NewExternalAddr(addr)
            }
            Some("<expired-external-addr") => {
                let addr = parts.next().unwrap().parse()?;
                Self::ExpiredExternalAddr(addr)
            }
            Some("<discovered") => {
                let peer = parts.next().unwrap().parse()?;
                Self::Discovered(peer)
            }
            Some("<unreachable") => {
                let peer = parts.next().unwrap().parse()?;
                Self::Unreachable(peer)
            }
            Some("<connected") => {
                let peer = parts.next().unwrap().parse()?;
                Self::Connected(peer)
            }
            Some("<disconnected") => {
                let peer = parts.next().unwrap().parse()?;
                Self::Disconnected(peer)
            }
            Some("<subscribed") => {
                let peer = parts.next().unwrap().parse()?;
                let topic = parts.next().unwrap().to_string();
                Self::Subscribed(peer, topic)
            }
            Some("<unsubscribed") => {
                let peer = parts.next().unwrap().parse()?;
                let topic = parts.next().unwrap().to_string();
                Self::Unsubscribed(peer, topic)
            }
            Some("<block") => {
                let cid = parts.next().unwrap().parse()?;
                let str_data = parts.next().unwrap();
                let mut data = Vec::with_capacity(str_data.len() / 2);
                for chunk in str_data.as_bytes().chunks(2) {
                    let s = std::str::from_utf8(chunk)?;
                    data.push(u8::from_str_radix(s, 16)?);
                }
                let block = Block::new(cid, data)?;
                Self::Block(block)
            }
            Some("<flushed") => Self::Flushed,
            Some("<synced") => Self::Synced,
            Some("<bootstrapped") => Self::Bootstrapped,
            Some("<newhead") => {
                let id = parts.next().unwrap().parse()?;
                let offset = parts.next().unwrap().parse()?;
                Self::NewHead(id, offset)
            }
            _ => return Err(anyhow::anyhow!("invalid event `{}`", s)),
        })
    }
}
