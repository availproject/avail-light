use super::{behaviour, transport};
use libp2p::{Multiaddr, PeerId, Swarm};

/// State machine representing the network currently running.
pub struct Network {
    swarm: Swarm<behaviour::Behaviour>,
}

/// Event that can happen on the network.
#[derive(Debug)]
pub enum Event {
    /// Received a block announcement for specific blocks.
    BlocksAnnouncementReceived {
        /// List of encoded headers.
        headers: Vec<Vec<u8>>,
    },
}

/// Configuration for starting the network.
///
/// Internal to the `network` module.
pub(super) struct Config {
    pub(super) known_addresses: Vec<(PeerId, Multiaddr)>,
}

impl Network {
    pub(super) async fn start(config: Config) -> Self {
        let local_key_pair = libp2p::identity::Keypair::generate_ed25519();
        let local_public_key = local_key_pair.public();
        let local_peer_id = local_public_key.clone().into_peer_id();
        let (transport, _) = transport::build_transport(local_key_pair, false, None, true);
        let behaviour = behaviour::Behaviour::new(
            "substrate-lite".to_string(),
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
    pub async fn announce_block(&mut self) {}

    /// Returns the next event that happened on the network.
    pub async fn next_event(&mut self) -> Event {
        loop {
            match self.swarm.next_event().await {
                // TODO: don't println
                ev => println!("{:?}", ev),
            }
        }
    }
}
