// TODO: remove this module

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
