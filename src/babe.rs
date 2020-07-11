//! BABE consensus.
//!
//! BABE, or Blind Assignment for Blockchain Extension, is the consensus algorithm used by
//! Polkadot in order to determine who is authorized to generate a block.
//!
//! References:
//!
//! - https://research.web3.foundation/en/latest/polkadot/BABE/Babe.html
//!
//! # Overview of BABE
//!
//! In the BABE algorithm, time is divided into non-overlapping **epochs**, themselves divided
//! into **slots**. How long an epoch and a slot are is determined by calling the
//! `BabeApi_configuration` runtime entry point.
//!
//! > **Note**: As example values, in the Polkadot genesis, a slot lasts for 6 seconds and an
//! >           epoch consists of 600 slots (in other words, one hour).
//!
//! Every block that is produced must belong to a specific slot. This slot number can be found in
//! the header of every single block with the exception of the genesis block. Slots are numbered,
//! and the genesis block implicitly belongs to slot 0.
//! While every block must belong to a specific block, the opposite is not necessarily true: not
//! all slots lead to a block being produced.
//!
//! Nodes can estimate what the current slot is (in other words, the slot corresponding to "now")
//! by looking at the latest received block. Each block, alongside with its slot number, includes
//! a "timestamp" intrinsic indicating the clock value when it has been generated.
//! TODO: write more about time maybe ^
//!
//! The header of first block produced after a transition to a new epoch must contain a log entry
//! indicating the public keys that are allowed to sign blocks during that epoch, alongside with
//! a weight for each of them, and a "randomness value".
//!
//! > **Note**: The way the list of authorities and their weights is determined is out of scope of
//! >           this code, but it normally corresponds to the list of validators and how much
//! >           stake is available to them.
//!
//! In order to produce a block, one must generate, using a
//! [VRF (Verifiable Random Function)](https://en.wikipedia.org/wiki/Verifiable_random_function),
//! and based on the slot number, genesis hash, and aformentioned "randomness value",
//! a number whose value is lower than a certain threshold.
//!
//! The number that has been generated must be included in the header of the block, alongside with
//! the proof that can be verified using one of the public keys allowed to generate blocks in that
//! epoch. The weight associated to that public key determines the allowed threshold.
//!
//! The "randomess value" of an epoch `N` is calculated by combining the generated numbers of all
//! the blocks of the epoch `N - 2`.
//!
//! TODO: read about and explain the secondary slot stuff
//!
//! ## Chain selection
//!
//! The "best" block of a chain in the BABE algorithm is the one with the highest slot number.
//! If there exists multiple blocks on the same slot, the best block is one with the most claimed
//! primary slots. In other words, is two blocks have the same parent, but one is a primary slot
//! claim and the other is a secondary slot claim, we prefer the one with the primary slot claim.
//!
//! Keep in mind that there can still be draws in terms of primary slot claims numbers, in which
//! case the winning block is the one upon which the next block author builds upon.
//!
//! # Runtime
//!
//! The runtime of a chain on which BABE operates provides a couple of entry points:
//!
//! - `BabeApi_configuration`, providing the configuration for BABE at the genesis block.
//!

mod definitions;

/// Verifies whether a block header.
pub fn verify_header() {}


/*
sp_api::decl_runtime_apis! {
    /// API necessary for block authorship with BABE.
    #[api_version(2)]
    pub trait BabeApi {
        /// Return the genesis configuration for BABE. The configuration is only read on genesis.
        fn configuration() -> BabeGenesisConfiguration;

        /// Returns the slot number that started the current epoch.
        fn current_epoch_start() -> SlotNumber;

        /// Generates a proof of key ownership for the given authority in the
        /// current epoch. An example usage of this module is coupled with the
        /// session historical module to prove that a given authority key is
        /// tied to a given staking identity during a specific session. Proofs
        /// of key ownership are necessary for submitting equivocation reports.
        /// NOTE: even though the API takes a `slot_number` as parameter the current
        /// implementations ignores this parameter and instead relies on this
        /// method being called at the correct block height, i.e. any point at
        /// which the epoch for the given slot is live on-chain. Future
        /// implementations will instead use indexed data through an offchain
        /// worker, not requiring older states to be available.
        fn generate_key_ownership_proof(
            slot_number: SlotNumber,
            authority_id: AuthorityId,
        ) -> Option<OpaqueKeyOwnershipProof>;

        /// Submits an unsigned extrinsic to report an equivocation. The caller
        /// must provide the equivocation proof and a key ownership proof
        /// (should be obtained using `generate_key_ownership_proof`). The
        /// extrinsic will be unsigned and should only be accepted for local
        /// authorship (not to be broadcast to the network). This method returns
        /// `None` when creation of the extrinsic fails, e.g. if equivocation
        /// reporting is disabled for the given runtime (i.e. this method is
        /// hardcoded to return `None`). Only useful in an offchain context.
        fn submit_report_equivocation_unsigned_extrinsic(
            equivocation_proof: EquivocationProof<Block::Header>,
            key_owner_proof: OpaqueKeyOwnershipProof,
        ) -> Option<()>;
    }
*/
