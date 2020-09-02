use core::{fmt, future::Future, pin::Pin};
use hashbrown::HashMap;
use rand::{seq::IteratorRandom as _, SeedableRng as _};

pub use connections_pool::RequestId;
pub use libp2p::{Multiaddr, PeerId};

mod connections_pool;
mod multistream_select;
mod noise;
mod yamux;

/// Configuration to provide when building a [`Network`].
pub struct Config<PIter> {
    /// List of notification protocols.
    pub notification_protocols: PIter,

    /// List of protocol names for request-response protocols.
    pub request_protocols: Vec<String>,

    /// Seed used by the PRNG (Pseudo-Random Number Generator) that selects
    /// which task to assign to each new connection.
    ///
    /// You are encouraged to use something like `rand::random()` to fill this
    /// field, except in situations where determinism/reproducibility is
    /// desired.
    pub pool_selection_randomness_seed: u64,

    /// Executor to spawn background tasks.
    pub tasks_executor: Box<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send>,
}

/// Collection of network connections.
pub struct Network<T, TRq, P> {
    pools: slab::Slab<connections_pool::ConnectionsPool<T, TRq, P>>,

    /// For each `PeerId` we're connected to, the pool that handles it.
    // TODO: we would actually benefit from the SipHasher here, considering that PeerIds are determined by the remote
    peers_pools: HashMap<PeerId, usize, fnv::FnvBuildHasher>,

    /// PRNG used to select the source to start a query with.
    pool_selection_rng: rand_chacha::ChaCha8Rng,
}

impl<T, TRq, P> Network<T, TRq, P> {
    /// Initializes the [`Network`].
    pub fn new(config: Config<impl Iterator<Item = P>>) -> Self {
        Network {
            pools: slab::Slab::new(),
            peers_pools: Default::default(),
            pool_selection_rng: rand_chacha::ChaCha8Rng::seed_from_u64(
                config.pool_selection_randomness_seed,
            ),
        }
    }

    /// Emits a request towards a certain peer.
    pub async fn start_request(
        &mut self,
        target: &PeerId,
        protocol_name: &str,
        user_data: TRq,
        payload: impl Into<Vec<u8>>,
    ) -> Result<RequestId, StartRequestError> {
        let pool_id = *self
            .peers_pools
            .get(target)
            .ok_or(StartRequestError::NotConnected)?;

        Ok(self.pools[pool_id]
            .start_request(target, protocol_name, user_data, payload)
            .await)
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

/// Event that happened on the [`Network`].
#[derive(Debug)]
pub enum Event<'a, T, TRq, P> {
    IncomingConnection {
        /// Address of the local listening endpoint.
        local_listener: Multiaddr,
        send_back_address: Multiaddr,
        accept: Accept<'a, T, TRq, P>,
    },
    RequestFinished {
        peer: &'a mut P,
    },
    Notification {
        user_data: &'a mut T,
        protocol: &'a mut P,
        payload: NotificationPayload<'a>,
    },
}

/// Accepts an incoming connection.
#[must_use]
pub struct Accept<'a, T, TRq, P> {
    network: &'a mut Network<T, TRq, P>,
}

impl<'a, T, TRq, P> Accept<'a, T, TRq, P> {
    /// Accepts the incoming connection.
    pub fn accept(self, user_data: T) {
        todo!()
    }
}

impl<'a, T, TRq, P> fmt::Debug for Accept<'a, T, TRq, P> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Accept").finish()
    }
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

#[derive(Debug, derive_more::Display)]
pub enum StartRequestError {
    NotConnected,
}
