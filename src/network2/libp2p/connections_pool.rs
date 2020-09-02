use core::fmt;
use hashbrown::HashMap;

pub use libp2p::{Multiaddr, PeerId};

/// Configuration to provide when building a [`ConnectionsPool`].
pub struct Config<PIter> {
    /// List of notification protocols.
    pub notification_protocols: PIter,

    /// List of protocol names for request-response protocols.
    pub request_protocols: Vec<String>,

    /// Pre-allocated capacity for the list of connections.
    pub connections_capacity: usize,
}

/// Collection of network connections.
pub struct ConnectionsPool<T, TRq, P> {
    // TODO: we would actually benefit from the SipHasher here, considering that PeerIds are determined by the remote
    connections: HashMap<PeerId, T, fnv::FnvBuildHasher>,
    protocols: Vec<P>,
    requests: Vec<TRq>,
}

impl<T, TRq, P> ConnectionsPool<T, TRq, P> {
    /// Initializes the [`ConnectionsPool`].
    pub fn new(config: Config<impl Iterator<Item = P>>) -> Self {
        ConnectionsPool {
            connections: HashMap::with_capacity_and_hasher(config.connections_capacity, Default::default()),
            protocols: config.notification_protocols.collect(),
            requests: Vec::new(),
        }
    }

    /// Emits a request towards a certain peer.
    pub async fn start_request(&mut self, target: &PeerId, protocol_name: &str, user_data: TRq, payload: impl Into<Vec<u8>>) -> RequestId {
        todo!()
    }

    /// Cancels a request. No [`Event::RequestFinished`] will be generated.
    pub fn cancel_request(&mut self, request_id: RequestId) -> TRq {
        todo!()
    }

    /// 
    pub async fn write_notification(&mut self, target: &PeerId, protocol_name: &str) {
        todo!()
    }

    pub async fn next_event<'a>(&'a mut self) -> Event<'a, T, TRq, P> {
        todo!()
    }
}

/// Identifier for an ongoing request within a [`ConnectionsPool`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct RequestId(usize);

/// Event that happened on the [`ConnectionsPool`].
#[derive(Debug)]
pub enum Event<'a, T, TRq, P> {
    RequestFinished {
        peer: &'a mut P,
        user_data: TRq,
    },
    Notification {
        user_data: &'a mut T,
        protocol: &'a mut P,
        payload: NotificationPayload<'a>,
    },
}


/// Notification data.
pub struct NotificationPayload<'a> {
    foo: &'a mut (),
}

impl<'a> AsRef<[u8]> for NotificationPayload<'a> {
    fn as_ref(&self) -> &[u8] {
        todo!()
    }
}

impl<'a> fmt::Debug for NotificationPayload<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("NotificationPayload").finish()
    }
}
