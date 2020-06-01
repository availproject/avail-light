use super::{behaviour, transport};

use fnv::FnvBuildHasher;
use hashbrown::HashSet;
use libp2p::{swarm::SwarmEvent, Multiaddr, PeerId, Swarm};
use smallvec::SmallVec;

/// State machine representing the network currently running.
pub struct Network {
    swarm: Swarm<behaviour::Behaviour>,
}

/// Event that can happen on the network.
#[derive(Debug)]
pub enum Event {
    /// An announcement about a block has been gossiped to us.
    BlockAnnounce(behaviour::BlockHeader),

    /// A blocks request started with [`Network::start_block_request`] has gotten a response.
    BlocksRequestFinished {
        id: behaviour::BlocksRequestId,
        result: Result<Vec<behaviour::BlockData>, ()>,
    },
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
        )
        .await;
        let swarm = Swarm::new(transport, behaviour, local_peer_id);

        Network { swarm }
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

                SwarmEvent::ConnectionEstablished { .. } => {}
                SwarmEvent::ConnectionClosed { .. } => {}
                SwarmEvent::NewListenAddr(_) => {}
                SwarmEvent::ExpiredListenAddr(_) => {}
                SwarmEvent::UnreachableAddr { .. } => {}
                SwarmEvent::Dialing(peer_id) => {}
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
