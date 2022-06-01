extern crate anyhow;
extern crate async_std;
extern crate ed25519_dalek;
extern crate ipfs_embed;
extern crate libipld;
extern crate rand;
extern crate rocksdb;
extern crate tempdir;
extern crate tokio;

use std::{str::FromStr, time::Duration};

use async_std::stream::StreamExt;
use ipfs_embed::{
	identity::ed25519::{Keypair, SecretKey},
	DefaultParams as IPFSDefaultParams, Ipfs, Multiaddr, NetworkConfig, PeerId, StorageConfig,
};

use crate::types::Event;

pub async fn make_client(
	seed: u64,
	port: u16,
	path: &str,
	bootstrap_nodes: &Vec<(PeerId, Multiaddr)>,
) -> anyhow::Result<Ipfs<IPFSDefaultParams>> {
	let sweep_interval = Duration::from_secs(600);
	let path_buf = std::path::PathBuf::from_str(path).unwrap();
	let storage = StorageConfig::new(Some(path_buf), None, 10, sweep_interval);
	let mut network = NetworkConfig::new(keypair(seed));
	network.mdns = None;

	let ipfs = Ipfs::<IPFSDefaultParams>::new(ipfs_embed::Config { storage, network }).await?;

	_ = ipfs.listen_on(format!("/ip4/127.0.0.1/tcp/{}", port).parse()?)?;

	if !bootstrap_nodes.is_empty() {
		ipfs.bootstrap(bootstrap_nodes).await?;
	} else {
		// If client is the first one on the network, wait for the second client ConnectionEstablished event to use it as bootstrap
		// DHT requires boostrap to complete in order to be able to insert new records
		let node = ipfs
			.swarm_events()
			.find_map(|event| {
				if let ipfs_embed::Event::ConnectionEstablished(peer_id, connected_point) = event {
					Some((peer_id, connected_point.get_remote_address().clone()))
				} else {
					None
				}
			})
			.await
			.expect("Connection established");
		ipfs.bootstrap(&[node]).await?;
	}

	Ok(ipfs)
}

pub async fn log_events(ipfs: Ipfs<IPFSDefaultParams>) {
	let mut events = ipfs.swarm_events();
	while let Some(event) = events.next().await {
		let event = match event {
			ipfs_embed::Event::NewListener(_) => Event::NewListener,
			ipfs_embed::Event::NewListenAddr(_, addr) => Event::NewListenAddr(addr),
			ipfs_embed::Event::ExpiredListenAddr(_, addr) => Event::ExpiredListenAddr(addr),
			ipfs_embed::Event::ListenerClosed(_) => Event::ListenerClosed,
			ipfs_embed::Event::NewExternalAddr(addr) => Event::NewExternalAddr(addr),
			ipfs_embed::Event::ExpiredExternalAddr(addr) => Event::ExpiredExternalAddr(addr),
			ipfs_embed::Event::Discovered(peer_id) => Event::Discovered(peer_id),
			ipfs_embed::Event::Unreachable(peer_id) => Event::Unreachable(peer_id),
			ipfs_embed::Event::Connected(peer_id) => Event::Connected(peer_id),
			ipfs_embed::Event::Disconnected(peer_id) => Event::Disconnected(peer_id),
			ipfs_embed::Event::Subscribed(peer_id, topic) => Event::Subscribed(peer_id, topic),
			ipfs_embed::Event::Unsubscribed(peer_id, topic) => Event::Unsubscribed(peer_id, topic),
			ipfs_embed::Event::Bootstrapped => Event::Bootstrapped,
			ipfs_embed::Event::NewInfo(peer_id) => Event::NewInfo(peer_id),
			_ => Event::Other, // TODO: Is there a purpose to handle those events?
		};
		log::trace!("Received event: {}", event);
	}
}

pub fn keypair(i: u64) -> Keypair {
	let mut keypair = [0; 32];
	keypair[..8].copy_from_slice(&i.to_be_bytes());
	let secret = SecretKey::from_bytes(keypair).unwrap();
	Keypair::from(secret)
}
