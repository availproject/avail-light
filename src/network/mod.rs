mod client;
mod event_loop;
mod stream;

use std::{fs, path::Path, str::FromStr, sync::Arc, time::Duration};

pub use client::Client;
use event_loop::{EventLoop, NetworkBehaviour};
pub use stream::Event;
use stream::NetworkStream;

use anyhow::{Context, Result};
use libp2p::{
	core::{
		either::EitherTransport,
		muxing::StreamMuxerBox,
		transport,
		upgrade::{SelectUpgrade, Version},
	},
	identify,
	identity::{self, ed25519, Keypair},
	kad::{
		store::{MemoryStore, MemoryStoreConfig},
		KademliaConfig,
	},
	metrics::Metrics,
	mplex::MplexConfig,
	noise::NoiseAuthenticated,
	pnet::{PnetConfig, PreSharedKey},
	swarm::SwarmBuilder,
	tcp::{GenTcpConfig, TokioTcpTransport},
	yamux::YamuxConfig,
	PeerId, Transport,
};
use tokio::sync::mpsc;
use tracing::info;

pub fn init(
	seed: Option<u8>,
	psk_path: &String,
	metrics: Metrics,
	port_reuse: bool,
) -> Result<(Client, Arc<NetworkStream>, EventLoop)> {
	// Create a public/private key pair, either based on a seed or random
	let id_keys = match seed {
		Some(seed) => {
			let mut bytes = [0u8; 32];
			bytes[0] = seed;
			let secret_key = ed25519::SecretKey::from_bytes(&mut bytes)
				.context("Error should only appear if length is wrong.")?;
			identity::Keypair::Ed25519(secret_key.into())
		},
		None => identity::Keypair::generate_ed25519(),
	};
	let local_peer_id = PeerId::from(id_keys.public());
	info!("Local peer id: {:?}", local_peer_id);

	// try to get psk
	let psk: Option<PreSharedKey> = get_psk(psk_path)?
		.map(|text| PreSharedKey::from_str(&text))
		.transpose()?;
	// create transport
	let transport = setup_transport(&id_keys, psk, port_reuse);

	// create swarm that manages peers and events
	let swarm = {
		let mut kad_cfg = KademliaConfig::default();
		kad_cfg.set_query_timeout(Duration::from_secs(5 * 60));
		let store_cfg = MemoryStoreConfig {
			max_records: 24000000, // ~2hrs
			max_value_bytes: 100,
			max_providers_per_key: 1,
			max_provided_keys: 100000,
		};
		let kad_store = MemoryStore::with_config(local_peer_id, store_cfg);
		let identify_cfg =
			identify::Config::new("/avail_kad/id/1.0.0".to_string(), id_keys.public());

		let behaviour = NetworkBehaviour::new(local_peer_id, kad_store, kad_cfg, identify_cfg)?;

		// Build the Swarm, connecting the lower transport logic with the
		// higher layer network behaviour logic
		SwarmBuilder::new(transport, behaviour, local_peer_id)
			// connection background tasks are spawned onto the tokio runtime
			.executor(Box::new(|fut| {
				tokio::spawn(fut);
			}))
			.build()
	};

	let (command_sender, command_receiver) = mpsc::channel(10000);
	let network_stream = Arc::new(NetworkStream::new());

	Ok((
		Client::new(command_sender),
		network_stream.clone(),
		EventLoop::new(swarm, command_receiver, metrics, network_stream),
	))
}

/// Read the pre-shared key file from the given directory
fn get_psk(location: &String) -> Result<Option<String>> {
	let path = Path::new(location);
	match fs::read_to_string(path) {
		Ok(text) => Ok(Some(text)),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
		Err(e) => Err(e.into()),
	}
}

fn setup_transport(
	key_pair: &Keypair,
	psk: Option<PreSharedKey>,
	port_reuse: bool,
) -> transport::Boxed<(PeerId, StreamMuxerBox)> {
	let noise = NoiseAuthenticated::xx(&key_pair).unwrap();

	let base_transport =
		TokioTcpTransport::new(GenTcpConfig::default().nodelay(true).port_reuse(port_reuse));
	let maybe_encrypted = match psk {
		Some(psk) => EitherTransport::Left(
			base_transport.and_then(move |socket, _| PnetConfig::new(psk).handshake(socket)),
		),
		None => EitherTransport::Right(base_transport),
	};

	maybe_encrypted
		.upgrade(Version::V1)
		.authenticate(noise)
		.multiplex(SelectUpgrade::new(
			YamuxConfig::default(),
			MplexConfig::new(),
		))
		.timeout(Duration::from_secs(20))
		.boxed()
}
