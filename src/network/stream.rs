use libp2p::{core::ConnectedPoint, PeerId};
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;

// Event enum encodes all used network event variants
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

// Structure that is used as an output
// for all network events that are of interest
#[derive(Default)]
pub struct NetworkEvents {
	swarm: Mutex<SwarmEvents>,
}

impl NetworkEvents {
	pub fn new() -> Self {
		Self::default()
	}

	// Stream function creates a new stream of network
	// events and registers a new channal for sending events
	pub async fn stream(&self) -> ReceiverStream<Event> {
		let mut swarm = self.swarm.lock().await;
		swarm.stream()
	}

	// Notify function is used to send network event to all listeners
	// through all send channals that are able to send, otherwise channal is
	// discarded
	pub async fn notify(&self, event: Event) {
		let mut swarm = self.swarm.lock().await;
		swarm.notify(event);
	}
}
