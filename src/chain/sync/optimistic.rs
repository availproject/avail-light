use super::super::blocks_tree;

use core::{borrow::Borrow, hash::Hash, iter};
use hashbrown::{hash_map::Entry, HashMap};

/// Configuration for the [`OptimisticSync`].
#[derive(Debug)]
pub struct Config {
    /// Configuration for the tree of blocks.
    pub chain_config: blocks_tree::Config,
}

pub struct OptimisticSync<TBlock, TSrc> {
    chain: blocks_tree::NonFinalizedTree<TBlock>,
    sources: HashMap<TSrc, SourceInfo>,

    /// Identifier to assign to the next request.
    next_request_id: RequestId,
}

#[derive(Debug)]
struct SourceInfo {
    pending_requests: arrayvec::ArrayVec<[PendingRequest; 1]>,
}

#[derive(Debug)]
struct PendingRequest {
    id: RequestId,
}

impl<TBlock, TSrc> OptimisticSync<TBlock, TSrc> {
    /// Builds a new [`OptimisticSync`].
    pub fn new(config: Config) -> Self {
        OptimisticSync {
            chain: blocks_tree::NonFinalizedTree::new(config.chain_config),
            sources: HashMap::new(),
            next_request_id: RequestId(0),
        }
    }
}

impl<TBlock, TSrc> OptimisticSync<TBlock, TSrc>
where
    TSrc: Eq + Hash,
{
    /// Inform the [`OptimisticSync`] of a new potential source of blocks.
    pub fn add_source(&mut self, source: TSrc) -> Result<(), AddSourceError> {
        let entry = match self.sources.entry(source) {
            Entry::Occupied(_) => return Err(AddSourceError::Duplicate),
            Entry::Vacant(e) => e,
        };

        entry.insert(SourceInfo {
            pending_requests: Default::default(),
        });

        Ok(())
    }

    /// Inform the [`OptimisticSync`] that a source of blocks is no longer available.
    ///
    /// This automatically cancels all the [`RequestId`]s that have been emitted for this source.
    /// This list of [`RequestId`]s is returned as part of this function.
    pub fn remove_source<Q>(
        &mut self,
        source: &Q,
    ) -> Result<impl Iterator<Item = RequestId>, RemoveSourceError>
    where
        TSrc: Borrow<Q>,
        Q: Hash + Eq,
    {
        if let Some(source_info) = self.sources.remove(source) {
            Ok(source_info.pending_requests.into_iter().map(|rq| rq.id))
        } else {
            Err(RemoveSourceError::UnknownSource)
        }
    }

    /// Returns an iterator that extracts all requests that need to be started and requests that
    /// need to be cancelled.
    pub fn requests_actions<'a>(&'a mut self) -> impl Iterator<Item = RequestAction<'a, TSrc>> {
        iter::from_fn(move || todo!())
    }

    /// Update the [`OptimisticSync`] with the outcome of a request.
    ///
    /// The [`OptimisticSync`] verifies that the response is correct and returns a
    /// [`FinishRequestOutcome`] describing what has happened.
    pub fn finish_request(
        &mut self,
        id: RequestId,
        outcome: Result<Vec<u8>, RequestFail>,
    ) -> FinishRequestOutcome {
        for (_, src_info) in &mut self.sources {
            let request = match src_info.pending_requests.iter().position(|rq| rq.id == id) {
                Some(pos) => src_info.pending_requests.remove(pos),
                None => continue,
            };

            // TODO: return ...
        }

        todo!()
    }
}

/// Identifier for an ongoing request in the [`OptimisticSync`].
#[derive(Debug, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct RequestId(u64);

/// Request that should be emitted towards a certain source.
#[derive(Debug)]
pub enum RequestAction<'a, TSrc> {
    /// A request must be emitted for the given source.
    Start {
        /// Identifier for the request. Must later be passed back to
        /// [`OptimisticSync::finish_request`].
        request_id: RequestId,
        /// Source where to request blocks from.
        source: &'a TSrc,
    },

    /// The given [`RequestId`] is no longer valid. The request can either be cancelled, or the
    /// request can let through while the response is ignored.
    Cancel(RequestId),
}

pub enum FinishRequestOutcome {}

/// Reason why a request has failed.
pub enum RequestFail {
    /// Requested blocks aren't available from this source.
    BlocksUnavailable,
}

/// Possible error when calling [`OptimisticSync::add_source`].
#[derive(Debug, derive_more::Display)]
pub enum AddSourceError {
    /// Source was already known.
    Duplicate,
}

/// Possible error when calling [`OptimisticSync::remove_source`].
#[derive(Debug, derive_more::Display)]
pub enum RemoveSourceError {
    /// Source wasn't in the list.
    UnknownSource,
}
