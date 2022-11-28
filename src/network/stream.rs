use libp2p::{core::ConnectedPoint, PeerId};
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;

#[derive(Debug, Clone)]
pub enum Event {
	ConnectionEstablished {
		peer_id: PeerId,
		endpoint: ConnectedPoint,
	},
}

#[derive(Default)]
struct SwarmEvents {
	senders: Vec<mpsc::Sender<Event>>,
}

impl SwarmEvents {
	fn stream(&mut self) -> ReceiverStream<Event> {
		let (tx, rx) = mpsc::channel(1000);
		self.senders.push(tx);
		ReceiverStream::new(rx)
	}

	fn notify(&mut self, event: Event) {
		self.senders.retain(|tx| tx.try_send(event.clone()).is_ok());
	}
}

#[derive(Default)]
pub struct NetworkEvents {
	swarm: Mutex<SwarmEvents>,
}

impl NetworkEvents {
	pub fn new() -> Self {
		Self::default()
	}

	pub async fn stream(&self) -> ReceiverStream<Event> {
		let mut swarm = self.swarm.lock().await;
		swarm.stream()
	}

	pub async fn notify(&self, event: Event) {
		let mut swarm = self.swarm.lock().await;
		swarm.notify(event);
	}
}
