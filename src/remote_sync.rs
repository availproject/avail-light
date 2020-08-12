//! State machine for syncing blocks from a remote.

use core::hash::Hash;

pub struct RemoteSync<TSrc> {
    remotes: Vec<TSrc>,
}

impl<TSrc> RemoteSync<TSrc>
where
    TSrc: PartialEq + Eq + Hash,
{
    pub fn new() -> Self {
        RemoteSync {
            remotes: Vec::new(),
        }
    }

    /// Changes the state of the [`RemoteSync`] to account for a new block announce received from
    /// one of the sources.
    pub fn block_announce(&mut self, source: &TSrc, hash: [u8; 32]) {
        todo!()
    }

    pub fn add_source(&mut self, source: TSrc, chain_head_hash: [u8; 32], chain_head_number: u64) {
        todo!()
    }

    pub fn remove_source(&mut self, source: &TSrc) {
        todo!()
    }
}

impl<TSrc> Default for RemoteSync<TSrc>
where
    TSrc: PartialEq + Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}
