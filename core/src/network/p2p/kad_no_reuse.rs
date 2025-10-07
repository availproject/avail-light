use libp2p::{
	kad::{self, Behaviour as KademliaBehaviour},
	swarm::{
		dial_opts::DialOpts, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler,
		THandlerInEvent, THandlerOutEvent, ToSwarm,
	},
	Multiaddr, PeerId,
};
use std::task::{Context, Poll};

/// A wrapper around Kademlia that disables port reuse on dial operations
pub struct NoReuse<TStore> {
	inner: KademliaBehaviour<TStore>,
}

impl<TStore> NoReuse<TStore> {
	pub fn new(inner: KademliaBehaviour<TStore>) -> Self {
		Self { inner }
	}
}

impl<TStore> NetworkBehaviour for NoReuse<TStore>
where
	TStore: kad::store::RecordStore + Send + 'static,
{
	type ConnectionHandler = <KademliaBehaviour<TStore> as NetworkBehaviour>::ConnectionHandler;
	type ToSwarm = <KademliaBehaviour<TStore> as NetworkBehaviour>::ToSwarm;

	fn handle_established_inbound_connection(
		&mut self,
		connection_id: ConnectionId,
		peer: PeerId,
		local_addr: &Multiaddr,
		remote_addr: &Multiaddr,
	) -> Result<THandler<Self>, ConnectionDenied> {
		self.inner.handle_established_inbound_connection(
			connection_id,
			peer,
			local_addr,
			remote_addr,
		)
	}

	fn handle_established_outbound_connection(
		&mut self,
		connection_id: ConnectionId,
		peer: PeerId,
		addr: &Multiaddr,
		role_override: libp2p::core::Endpoint,
		port_use: libp2p::core::transport::PortUse,
	) -> Result<THandler<Self>, ConnectionDenied> {
		self.inner.handle_established_outbound_connection(
			connection_id,
			peer,
			addr,
			role_override,
			port_use,
		)
	}

	fn on_swarm_event(&mut self, event: FromSwarm) {
		self.inner.on_swarm_event(event)
	}

	fn on_connection_handler_event(
		&mut self,
		peer_id: PeerId,
		connection_id: ConnectionId,
		event: THandlerOutEvent<Self>,
	) {
		self.inner
			.on_connection_handler_event(peer_id, connection_id, event)
	}

	fn poll(
		&mut self,
		cx: &mut Context<'_>,
	) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
		match self.inner.poll(cx) {
			Poll::Ready(ToSwarm::Dial { opts }) => {
				let opts = match opts.get_peer_id() {
					Some(id) => DialOpts::peer_id(id).allocate_new_port().build(),
					None => opts,
				};

				Poll::Ready(ToSwarm::Dial { opts })
			},
			Poll::Ready(other) => Poll::Ready(other),
			Poll::Pending => Poll::Pending,
		}
	}
}

// Delegate common Kademlia methods to the inner behaviour
impl<TStore> std::ops::Deref for NoReuse<TStore> {
	type Target = KademliaBehaviour<TStore>;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl<TStore> std::ops::DerefMut for NoReuse<TStore> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.inner
	}
}
