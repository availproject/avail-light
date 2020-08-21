//! State of the finalization of a chain.
//!
//! The [`FinalizationChainState`] struct holds the state of the chain when it comes to
//! finalization.

pub struct FinalizationChainState {
    finalized_height: u64,
    scheduled_changes: Vec<(u64, Vec<Authority>)>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Authority {
    /// Ed25519 public key.
    pub public_key: [u8; 32],

    /// Arbitrary number indicating the weight of the authority.
    ///
    /// This value can only be compared to other weight values.
    pub weight: u64,
}

impl FinalizationChainState {
    pub fn set_finalized_block(&mut self, new_height: u64) {
        self.finalized_height = new_height;
    }

    pub fn add_scheduled_change(
        &mut self,
        triggered_block_height: u64,
        authorities: Vec<Authority>,
    ) {
    }
}
