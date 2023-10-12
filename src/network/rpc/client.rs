use anyhow::{Context, Result};
use avail_subxt::utils::H256;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
pub struct Client {
	command_sender: mpsc::Sender<Command>,
}

impl Client {
	pub fn new(command_sender: mpsc::Sender<Command>) -> Self {
		Self { command_sender }
	}

	pub async fn get_block_hash(self, block: u32) -> Result<H256> {
		let (response_sender, response_receiver) = oneshot::channel();
		self.command_sender
			.send(Command::GetBlockHash { block })
			.await
			.context("RPC Command Receiver not be dropped")?;
		response_receiver
			.await
			.context("RPC Command Sender not to be dropped.")?
	}
}

pub enum Command {
	GetBlockHash { block: u32 },
}
