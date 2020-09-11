//! State machine handling a single libp2p connection.

pub struct MultiplexedConnection<T> {
    substreams: slab::Slab<Substream<T>>,
}

struct Substream<T> {
    user_data: T,
}

impl<T> MultiplexedConnection<T> {
    pub fn new() -> Self {
        MultiplexedConnection {
            substreams: slab::Slab::new(),
        }
    }

    pub fn add_substream(&mut self, user_data: T) -> SubstreamId {
        SubstreamId(self.substreams.insert(Substream { user_data }))
    }

    pub fn remove_substream(&mut self, id: SubstreamId) {
        self.substreams.remove(id.0);
    }

    pub fn inject_data(&mut self, id: SubstreamId, data: &[u8]) {
        todo!()
    }
}

impl<T> Default for MultiplexedConnection<T> {
    fn default() -> Self {
        MultiplexedConnection::new()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct SubstreamId(usize);
