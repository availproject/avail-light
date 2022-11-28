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
struct EventStream {
	senders: Vec<mpsc::Sender<Event>>,
}

impl EventStream {
	fn stream(&mut self) -> ReceiverStream<Event> {
		let (tx, rx) = mpsc::channel(1000);
		self.senders.push(tx);
		ReceiverStream::new(rx)
	}

	fn notify(&mut self, event: Event) {
		self.senders.retain(|tx| tx.try_send(event.clone()).is_ok());
	}
}

pub struct NetworkStream {
	stream: Mutex<EventStream>,
}

impl NetworkStream {
	pub fn new() -> Self {
		Self {
			stream: Mutex::new(EventStream {
				senders: Default::default(),
			}),
		}
	}

	pub async fn stream(&self) -> ReceiverStream<Event> {
		let mut stream = self.stream.lock().await;
		stream.stream()
	}

	pub async fn notify(&self, event: Event) {
		let mut stream = self.stream.lock().await;
		stream.notify(event);
	}
}
