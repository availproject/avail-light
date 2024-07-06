use anyhow::{Context, Result};
use libp2p::{Multiaddr, PeerId};
use tokio::sync::{mpsc, oneshot};

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

    pub async fn bootstrap(&self) -> Result<()> {
        // bootstrapping is impossible on an empty DHT table
        // at least one node is required to be known, so check
        let counted_peers = self.count_dht_entries().await?;
        // for a bootstrap to succeed, we need at least 1 peer in our DHT
        if counted_peers < 1 {
            // we'll have to wait, until some one successfully connects us
            let (peer_id, multiaddr) = self.wait_connection(None).await?;
            // add that peer to have someone to bootstrap with
            self.add_address(peer_id, multiaddr).await?;
        }

        // proceed to bootstrap only if connected with someone
        let (boot_res_sender, boot_res_receiver) = oneshot::channel();
        self.command_sender
            .send(Command::Bootstrap {
                response_sender: boot_res_sender,
            })
            .await
            .context("Command receiver should not be dropped while bootstrapping.")?;
        boot_res_receiver
            .await
            .context("Sender not to be dropped while bootstrapping.")?
    }

    async fn wait_connection(&self, peer_id: Option<PeerId>) -> Result<(PeerId, Multiaddr)> {
        let (connection_res_sender, connection_res_receiver) = oneshot::channel();
        self.command_sender
            .send(Command::WaitConnection {
                peer_id,
                response_sender: connection_res_sender,
            })
            .await
            .context("Command receiver should not be dropped while waiting on connection.")?;
        Ok(connection_res_receiver.await?)
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
}

#[derive(Debug)]
pub enum Command {
    StartListening {
        addr: Multiaddr,
        response_sender: oneshot::Sender<Result<()>>,
    },
    AddAddress {
        peer_id: PeerId,
        multiaddr: Multiaddr,
        response_sender: oneshot::Sender<Result<()>>,
    },
    Bootstrap {
        response_sender: oneshot::Sender<Result<()>>,
    },
    WaitConnection {
        peer_id: Option<PeerId>,
        response_sender: oneshot::Sender<(PeerId, Multiaddr)>,
    },
    CountDHTPeers {
        response_sender: oneshot::Sender<usize>,
    },
    GetMultiaddress {
        response_sender: oneshot::Sender<Option<Multiaddr>>,
    },
}
