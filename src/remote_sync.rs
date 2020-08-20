//! State machine for syncing blocks from remotes.
//!
//! The [`RemoteSync`] struct manages a list of *sources* representing potential sources of
//! blocks.
//!
//! The words "source" or "remote" typically designate a node on the peer-to-peer network, but
//! this module is in no way specific to downloading blocks over a network.
//!
//! # Context
//!
//! In a blockchain network, blocks can appear at any time.
// TODO: finish doc ^

use crate::fork_tree;

use core::hash::Hash;
use hashbrown::HashMap;

/// State machine for syncing blocks from a remote.
pub struct RemoteSync<TSrc> {
    /// List of remotes added through [`RemoteSync::add_source`].
    remotes: HashMap<TSrc, PeerInfo, fnv::FnvBuildHasher>,
    /// List of forks that need to be downloaded.
    forks: fork_tree::ForkTree<BlockToDownload<TSrc>>,
}

struct PeerInfo {
    chain_head_hash: [u8; 32],
    chain_head_number: u64,
}

struct BlockToDownload<TSrc> {
    hash: [u8; 32],
    number: u64,
    sources: Vec<TSrc>,
}

impl<TSrc> RemoteSync<TSrc>
where
    TSrc: PartialEq + Eq + Hash,
{
    pub fn new() -> Self {
        RemoteSync {
            remotes: HashMap::default(),
            forks: Default::default(),
        }
    }

    /// Changes the state of the [`RemoteSync`] to account for a new block announce received from
    /// one of the sources.
    ///
    /// Returns an error if the source doesn't exist.
    pub fn block_announce(&mut self, announce: BlockAnnounce<TSrc>) -> Result<(), ()> {
        let remote = self.remotes.get_mut(announce.source).ok_or(())?;
        todo!()
    }

    /// Adds a source of blocks to the collection.
    pub fn add_source(&mut self, source: TSrc, chain_head_hash: [u8; 32], chain_head_number: u64) {
        /*self.remotes.insert(source, PeerInfo {

        });*/
        todo!()
    }

    /// Removes a source previously added with [`RemoteSync::add_source`].
    ///
    /// Returns an error if the source didn't exist.
    pub fn remove_source(&mut self, source: &TSrc) -> Result<(), ()> {
        if self.remotes.remove(&source).is_some() {
            Ok(())
        } else {
            Err(())
        }
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

/// Information about a block announcement.
#[derive(Debug, Clone)]
pub struct BlockAnnounce<'a, TSrc> {
    pub source: &'a TSrc,
    pub number: u64,
    pub parent_hash: [u8; 32],
    pub hash: [u8; 32],
}
