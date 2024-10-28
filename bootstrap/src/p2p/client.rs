use anyhow::{Context, Result};
use libp2p::{Multiaddr, PeerId};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub struct PeerInfo {
	pub peer_id: String,
	pub local_listeners: Vec<String>,
	pub external_listeners: Vec<String>,
}

#[derive(Clone)]
pub struct Client {
	command_sender: mpsc::Sender<Command>,
}

impl Client {
	pub fn new(command_sender: mpsc::Sender<Command>) -> Self {
		Self { command_sender }
	}

	pub async fn start_listening(&self, addr: Multiaddr) -> Result<()> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::StartListening {
				addr,
				response_sender,
			})
			.await
			.context("Command receiver should not be dropped")?;
		response_receiver
			.await
			.context("Sender not to be dropped")?
	}

	pub async fn add_address(&self, peer_id: PeerId, multiaddr: Multiaddr) -> Result<()> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::AddAddress {
				peer_id,
				multiaddr,
				response_sender,
			})
			.await
			.context("Command receiver should not be dropped.")?;
		response_receiver
			.await
			.context("Sender not to be dropped.")?
	}

	pub async fn dial_peer(&self, peer_id: PeerId, peer_address: Multiaddr) -> Result<()> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::DialPeer {
				peer_id,
				peer_address,
				response_sender,
			})
			.await
			.context("Command receiver should not be dropped.")?;
		response_receiver
			.await
			.context("Sender not to be dropped.")?
	}

	// Checks if bootstraps are available and adds them to the routing table automatically triggering bootstrap process
	pub async fn add_bootstrap_nodes(&self, nodes: Vec<(PeerId, Multiaddr)>) -> Result<()> {
		for (peer, addr) in nodes {
			self.dial_peer(peer, addr.clone())
				.await
				.context("Dialing Bootstrap peer failed.")?;
			self.add_address(peer, addr.clone()).await?;
		}

		Ok(())
	}

	pub async fn count_dht_entries(&self) -> Result<usize> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::CountDHTPeers { response_sender })
			.await
			.context("Command receiver not to be dropped.")?;
		response_receiver.await.context("Sender not to be dropped.")
	}

	pub async fn get_multiaddress(&self) -> Result<Option<Multiaddr>> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetMultiaddress { response_sender })
			.await
			.context("Command receiver not to be dropped.")?;
		response_receiver.await.context("Sender not to be dropped.")
	}

	pub async fn get_local_peer_info(&self) -> Result<PeerInfo> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetLocalPeerInfo { response_sender })
			.await
			.context("Command receiver not to be dropped.")?;
		response_receiver.await.context("Sender not to be dropped.")
	}
}

#[derive(Debug)]
pub enum Command {
	StartListening {
		addr: Multiaddr,
		response_sender: oneshot::Sender<Result<()>>,
	},
	DialPeer {
		peer_id: PeerId,
		peer_address: Multiaddr,
		response_sender: oneshot::Sender<Result<()>>,
	},
	AddAddress {
		peer_id: PeerId,
		multiaddr: Multiaddr,
		response_sender: oneshot::Sender<Result<()>>,
	},
	CountDHTPeers {
		response_sender: oneshot::Sender<usize>,
	},
	GetMultiaddress {
		response_sender: oneshot::Sender<Option<Multiaddr>>,
	},
	GetLocalPeerInfo {
		response_sender: oneshot::Sender<PeerInfo>,
	},
}
