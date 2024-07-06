use allow_block_list::BlockedPeers;
use anyhow::{Context, Result};
use libp2p::{
    autonat, identify,
    identity::{self, Keypair},
    kad::{self, store::MemoryStore, Mode},
    noise, ping,
    swarm::NetworkBehaviour,
    tcp, yamux, PeerId, SwarmBuilder,
};
use multihash::Hasher;
use tokio::sync::mpsc;

mod client;
mod event_loop;

use crate::{
    p2p::client::{Client, Command},
    types::{LibP2PConfig, SecretKey},
};
use event_loop::EventLoop;
use libp2p_allow_block_list as allow_block_list;
use tracing::info;

#[derive(NetworkBehaviour)]
pub struct Behaviour {
    kademlia: kad::Behaviour<MemoryStore>,
    identify: identify::Behaviour,
    auto_nat: autonat::Behaviour,
    ping: ping::Behaviour,
    blocked_peers: allow_block_list::Behaviour<BlockedPeers>,
}

pub async fn init(
    cfg: LibP2PConfig,
    id_keys: Keypair,
    is_ws_transport: bool,
) -> Result<(Client, EventLoop)> {
    let local_peer_id = PeerId::from(id_keys.public());
    info!(
        "Local Peer ID: {:?}. Public key: {:?}.",
        local_peer_id,
        id_keys.public()
    );

    // create Identify Protocol Config
    let identify_cfg =
        identify::Config::new(cfg.identify.protocol_version.clone(), id_keys.public())
            .with_agent_version(cfg.identify.agent_version.to_string());

    // create AutoNAT Server Config
    let autonat_cfg = autonat::Config {
        only_global_ips: cfg.autonat.only_global_ips,
        throttle_clients_global_max: cfg.autonat.throttle_clients_global_max,
        throttle_clients_peer_max: cfg.autonat.throttle_clients_peer_max,
        throttle_clients_period: cfg.autonat.throttle_clients_period,
        ..Default::default()
    };

    // create new Kademlia Memory Store
    let kad_store = MemoryStore::new(id_keys.public().to_peer_id());
    // create Kademlia Config
    let mut kad_cfg = kad::Config::default();
    kad_cfg
        .set_query_timeout(cfg.kademlia.query_timeout)
        .set_protocol_names(vec![cfg.kademlia.protocol_name]);

    // build the Swarm, connecting the lower transport logic with the
    // higher layer network behaviour logic
    let tokio_swarm = SwarmBuilder::with_existing_identity(id_keys.clone()).with_tokio();

    let mut swarm;

    let behaviour = |key: &identity::Keypair| {
        Ok(Behaviour {
            kademlia: kad::Behaviour::with_config(key.public().to_peer_id(), kad_store, kad_cfg),
            identify: identify::Behaviour::new(identify_cfg),
            auto_nat: autonat::Behaviour::new(local_peer_id, autonat_cfg),
            ping: ping::Behaviour::new(ping::Config::new()),
            blocked_peers: allow_block_list::Behaviour::default(),
        })
    };

    if is_ws_transport {
        swarm = tokio_swarm
            .with_websocket(noise::Config::new, yamux::Config::default)
            .await?
            .with_behaviour(behaviour)?
            .build()
    } else {
        swarm = tokio_swarm
            .with_tcp(
                tcp::Config::default().port_reuse(false).nodelay(false),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_dns()?
            .with_behaviour(behaviour)?
            .build()
    }

    // enable Kademlila Server mode
    swarm.behaviour_mut().kademlia.set_mode(Some(Mode::Server));

    // create channel for Event Loop Commands
    let (command_sender, command_receiver) = mpsc::channel::<Command>(1000);

    Ok((
        Client::new(command_sender),
        EventLoop::new(swarm, command_receiver, cfg.bootstrap_interval),
    ))
}

pub fn keypair(cfg: LibP2PConfig) -> Result<(Keypair, String)> {
    let keypair = match cfg.secret_key {
        // if seed is provided, generate secret key from seed
        Some(SecretKey::Seed { seed }) => {
            let digest = multihash::Sha3_256::digest(seed.as_bytes());
            Keypair::ed25519_from_bytes(digest).context("Error generating secret key from seed")?
        }
        // import secret key, if provided
        Some(SecretKey::Key { key }) => {
            let mut decoded_key = [0u8; 32];
            hex::decode_to_slice(key.into_bytes(), &mut decoded_key)
                .context("Error decoding secret key from config.")?;
            Keypair::ed25519_from_bytes(decoded_key).context("Error importing secret key.")?
        }
        // if neither seed nor secret key were provided,
        // generate secret key from random seed
        None => Keypair::generate_ed25519(),
    };

    let peer_id = PeerId::from(keypair.public()).to_string();
    Ok((keypair, peer_id))
}
