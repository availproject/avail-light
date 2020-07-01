use super::{behaviour, transport};

use alloc::boxed::Box;
use core::{future::Future, num::NonZeroUsize, pin::Pin};

use libp2p::{
    swarm::{SwarmBuilder, SwarmEvent},
    Multiaddr, PeerId, Swarm,
};

/// State machine representing the network currently running.
pub struct Network {
    swarm: Swarm<behaviour::Behaviour>,
}

/// Event that can happen on the network.
#[derive(Debug)]
pub enum Event {
    /// An announcement about a block has been gossiped to us.
    BlockAnnounce(behaviour::ScaleBlockHeader),

    /// A blocks request started with [`Network::start_block_request`] has gotten a response.
    BlocksRequestFinished {
        id: behaviour::BlocksRequestId,
        result: Result<Vec<behaviour::BlockData>, ()>,
    },

    /// Established at least one connection with the given peer.
    Connected(PeerId),
    /// No longer have any connection with the given peer.
    Disconnected(PeerId),
}

/// Configuration for starting the network.
///
/// Internal to the `network` module.
pub(super) struct Config {
    /// List of peer ids and their addresses that we know are part of the peer-to-peer network.
    pub(super) known_addresses: Vec<(PeerId, Multiaddr)>,
    /// Small string identifying the name of the chain, in order to detect incompatible nodes
    /// earlier.
    pub(super) chain_spec_protocol_id: Vec<u8>,
    /// How to spawn libp2p background tasks. If `None`, then a threads pool will be used by
    /// default.
    pub(super) executor: Option<Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>>,
    /// Hash of the genesis block of the local node.
    pub(super) local_genesis_hash: [u8; 32],
}

impl Network {
    pub(super) async fn start(config: Config) -> Self {
        let local_key_pair = libp2p::identity::Keypair::generate_ed25519();
        let local_public_key = local_key_pair.public();
        let local_peer_id = local_public_key.clone().into_peer_id();
        let (transport, _) = transport::build_transport(local_key_pair, false, None, true);
        let behaviour = behaviour::Behaviour::new(
            "substrate-lite".to_string(),
            config.chain_spec_protocol_id,
            local_public_key,
            config.known_addresses,
            true,
            true,
            50,
            // TODO: best hash != genesis_hash
            config.local_genesis_hash.clone().into(),
            config.local_genesis_hash.clone().into(),
        )
        .await;

        let swarm = {
            let mut builder = SwarmBuilder::new(transport, behaviour, local_peer_id)
                .peer_connection_limit(2)
                .notify_handler_buffer_size(NonZeroUsize::new(64).unwrap())
                .connection_event_buffer_size(256);
            if let Some(spawner) = config.executor {
                struct SpawnImpl<F>(F);
                impl<F: Fn(Pin<Box<dyn Future<Output = ()> + Send>>)> libp2p::core::Executor for SpawnImpl<F> {
                    fn exec(&self, f: Pin<Box<dyn Future<Output = ()> + Send>>) {
                        (self.0)(f)
                    }
                }
                builder = builder.executor(Box::new(SpawnImpl(spawner)));
            }
            builder.build()
        };

        Network { swarm }
    }

    /// Returns the [`PeerId`] of the local node.
    pub fn local_peer_id(&self) -> &PeerId {
        Swarm::local_peer_id(&self.swarm)
    }

    /// Sends out an announcement about the given block.
    pub async fn announce_block(&mut self) {
        unimplemented!()
    }

    pub async fn start_block_request(
        &mut self,
        config: behaviour::BlocksRequestConfig,
    ) -> behaviour::BlocksRequestId {
        self.swarm.send_block_data_request(config)
    }

    /// Returns the next event that happened on the network.
    pub async fn next_event(&mut self) -> Event {
        loop {
            match self.swarm.next_event().await {
                SwarmEvent::Behaviour(behaviour::BehaviourOut::BlockAnnounce(header)) => {
                    return Event::BlockAnnounce(header);
                }
                SwarmEvent::Behaviour(behaviour::BehaviourOut::BlocksResponse { id, result }) => {
                    return Event::BlocksRequestFinished { id, result };
                }

                SwarmEvent::ConnectionEstablished {
                    peer_id,
                    num_established,
                    ..
                } if num_established.get() == 1 => {
                    return Event::Connected(peer_id);
                }
                SwarmEvent::ConnectionEstablished { .. } => {}
                SwarmEvent::ConnectionClosed {
                    peer_id,
                    num_established: 0,
                    ..
                } => {
                    return Event::Disconnected(peer_id);
                }
                SwarmEvent::ConnectionClosed { .. } => {}
                SwarmEvent::NewListenAddr(_) => {}
                SwarmEvent::ExpiredListenAddr(_) => {}
                SwarmEvent::UnreachableAddr { .. } => {}
                SwarmEvent::Dialing(_peer_id) => {}
                SwarmEvent::IncomingConnection { .. } => {}
                SwarmEvent::IncomingConnectionError { .. } => {}
                SwarmEvent::BannedPeer { .. } => {}
                SwarmEvent::UnknownPeerUnreachableAddr { .. } => {}
                SwarmEvent::ListenerClosed { .. } => {}
                SwarmEvent::ListenerError { .. } => {}
            }
        }
    }
}
