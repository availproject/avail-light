extern crate ipfs_embed;

use ipfs_embed::{Block, DefaultParams as IPFSDefaultParams, Multiaddr, PeerId, StreamId};

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
