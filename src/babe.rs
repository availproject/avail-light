//! BABE consensus.
//!
//! BABE, or Blind Assignment for Blockchain Extension, is the consensus algorithm used by
//! Polkadot in order to determine who is authorized to generate a block.
//!
//! Every block (with the exception of the genesis block) must contain, in its header, some data
//! that makes it possible to verify the correctness of the block.
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
//! The number that has been generated must be included in the header of the authored block,
//! alongside with the proof of the correct generation that can be verified using one of the
//! public keys allowed to generate blocks in that epoch. The weight associated to that public key
//! determines the allowed threshold.
//!
//! The "randomess value" of an epoch `N` is calculated by combining the generated numbers of all
//! the blocks of the epoch `N - 2`.
//!
//! TODO: read about and explain the secondary slot stuff
//!
//! ## Chain selection
//!
//! The "best" block of a chain in the BABE algorithm is the one with the highest slot number.
//! If there exists multiple blocks on the same slot, the best block is one with the highest number
//! of primary slots claimed. In other words, if two blocks have the same parent, but one is a
//! primary slot claim and the other is a secondary slot claim, we prefer the one with the primary
//! slot claim.
//!
//! Keep in mind that there can still be draws in terms of primary slot claims count, in which
//! case the winning block is the one upon which the next block author builds upon.
//!

use crate::executor;

mod definitions;
mod runtime;

/// Failed to verify a block.
#[derive(Debug, Clone, derive_more::Display)]
pub enum VerifyError {}

/// Configuration for [`verify_header`].
pub struct VerifyConfig<'a> {
    /// SCALE-encoded header of the block.
    pub scale_encoded_header: &'a [u8],
    // TODO:
    /*/// BABE configuration retrieved from the genesis block.
    ///
    /// Can be obtained by calling [`BabeGenesisConfiguration::from_runtime_code`] with the
    /// runtime of the genesis block.
    pub genesis_configuration: &'a BabeGenesisConfiguration,*/
}

/// Verifies whether a block header provides a correct proof of the legitimacy of the authorship.
pub fn verify_header(config: VerifyConfig) -> Result<(), VerifyError> {
    // TODO:
    Ok(())
}

/// BABE configuration of a chain, as extracted from the genesis block.
///
/// The way a chain configures BABE is stored in its runtime.
pub struct BabeGenesisConfiguration {}

impl BabeGenesisConfiguration {
    /// Retrieves the configuration from the given runtime code.
    ///
    /// Returns back the same virtual machine prototype as was passed as parameter.
    pub fn from_runtime_code(
        &self,
        vm: executor::WasmVmPrototype,
    ) -> (
        Result<Self, BabeChainConfigurationError>,
        executor::WasmVmPrototype,
    ) {
        /*let mut vm = vm.run_old(executor::FunctionToCall::BabeApiConfiguration)
            .map_err(BabeChainConfigurationError::VmInitialization).unwrap(); // TODO: don't unwrap, but must give back the VmPrototype

        let outcome = loop {
            match vm.state() {
                executor::State::ReadyToRun(r) => r.run(),
                // TODO: no, should be BabeApi thing
                executor::State::Finished(executor::Success::CoreVersion(version)) => {
                    break Ok(version.clone());
                }
                executor::State::Finished(_) => unreachable!(),
                executor::State::Trapped => break Err(()),

                // Since there are potential ambiguities we don't allow any storage access
                // or anything similar. The last thing we want is to have an infinite
                // recursion of runtime calls.
                _ => break Err(()),
            }
        };*/

        todo!()
    }
}

/// Error when retrieving the BABE configuration.
#[derive(Debug, derive_more::Display)]
pub enum BabeChainConfigurationError {
    /// Error when initializing the virtual machine.
    VmInitialization(executor::NewErr),
}
