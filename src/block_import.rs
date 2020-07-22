//! Block verification.
//!
//! When we receive a block from the network whose number is higher than the number of the best
//! block that we know of, we can potentially add this block to the chain and treat it as the new
//! best block.
//!
//! But before doing so, we must first verify whether the block is correct. In other words, that
//! all the extrinsics in the block can indeed be applied on top of its parent.
//!
//! Verifying a block consists of twp main steps:
//!
//! - Verifying the consensus layer to make sure that the author of the block was authorized to
//! produce it.
//! - Executing the block. This involves calling the `Core_execute_block` runtime function with
//! the header and body of the block for the runtime to verify that all the extrinsics are
//! correct.
//!

use crate::{babe, executor, header, trie::calculate_root};

use core::{cmp, convert::TryFrom as _, iter, time::Duration};
use futures::prelude::*;
use hashbrown::{HashMap, HashSet};
use parity_scale_codec::DecodeAll as _;

mod unsealed;

/// Configuration for a block verification.
// TODO: don't pass functions to the Config; instead, have a state-machine-like API
pub struct Config<'a, TBody, TPaAcc, TPaPref, TPaNe, TEGet> {
    /// Runtime used to check the new block. Must be built using the `:code` of the parent
    /// block.
    pub parent_runtime: executor::WasmVmPrototype,

    /// BABE configuration retrieved from the genesis block.
    ///
    /// See the documentation of [`babe::BabeGenesisConfiguration`] to know how to get this.
    pub babe_genesis_configuration: &'a babe::BabeGenesisConfiguration,

    /// Slot number of block #1. **Must** be provided, unless the block being verified is block
    /// #1 itself.
    ///
    /// Must be the value of [`Success::slot_number`] for block #1.
    pub block1_slot_number: Option<u64>,

    /// Time elapsed since [the Unix Epoch](https://en.wikipedia.org/wiki/Unix_time) (i.e.
    /// 00:00:00 UTC on 1 January 1970), ignoring leap seconds.
    pub now_from_unix_epoch: Duration,

    /// Header of the block to verify.
    ///
    /// The `parent_hash` field is the hash of the parent whose storage can be accessed through
    /// the other fields.
    pub block_header: header::HeaderRef<'a>,

    /// Body of the block to verify.
    pub block_body: TBody,

    /// Header of the parent of the block to verify.
    ///
    /// The hash of this header must be the one referenced in [`Config::block_header`].
    pub parent_block_header: header::HeaderRef<'a>,

    /// Function that returns the value in the parent's storage correpsonding to the key passed
    /// as parameter. Returns `None` if there is no value associated to this key.
    ///
    /// > **Note**: Returning `None` does *not* mean "unknown". It means "known to be empty".
    pub parent_storage_get: TPaAcc,

    /// Function that returns the keys in the parent's storage that start with the given prefix.
    pub parent_storage_keys_prefix: TPaPref,

    /// Function that returns the key in the parent's storage that immediately follows the one
    /// passed as parameter. Returns `None` if this is the last key.
    pub parent_storage_next_key: TPaNe,

    /// Function that returns the BABE epoch information from a given epoch number.
    pub babe_epoch_information_get: TEGet,

    /// Optional cache corresponding to the storage trie root hash calculation.
    pub top_trie_root_calculation_cache: Option<calculate_root::CalculationCache>,
}

/// Block successfully verified.
pub struct Success {
    /// Runtime that was passed by [`Config`].
    pub parent_runtime: executor::WasmVmPrototype,

    /// If `Some`, the block is the first block of a new BABE epoch. Returns the information about
    /// the epoch.
    pub babe_epoch_change: Option<babe::EpochChangeInformation>,

    /// Slot number the block belongs to.
    pub slot_number: u64,

    /// List of changes to the storage top trie that the block performs.
    pub storage_top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>>,

    /// Cache used for calculating the top trie root.
    pub top_trie_root_calculation_cache: calculate_root::CalculationCache,
    // TOOD: logs written by the runtime
}

/// Error that can happen during the verification.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Error while verifying the unsealed block.
    Unsealed(unsealed::Error),
    /// Failed to verify the authenticity of the block with the BABE algorithm.
    BabeVerification(babe::VerifyError),
}

/// Verifies whether a block is valid.
pub async fn verify_block<
    'a,
    TBody,
    TExt,
    TPaAcc,
    TPaAccOut,
    TPaPref,
    TPaPrefOut,
    TPaNe,
    TPaNeOut,
    TEGet,
>(
    config: Config<'a, TBody, TPaAcc, TPaPref, TPaNe, TEGet>,
) -> Result<Success, Error>
where
    TBody: ExactSizeIterator<Item = TExt> + Clone,
    TExt: AsRef<[u8]> + Clone,
    // TODO: ugh, we pass Vecs because of lifetime clusterfuck
    TPaAcc: Fn(Vec<u8>) -> TPaAccOut,
    TPaAccOut: Future<Output = Option<Vec<u8>>>,
    TPaPref: Fn(Vec<u8>) -> TPaPrefOut,
    TPaPrefOut: Future<Output = Vec<Vec<u8>>>,
    TPaNe: Fn(Vec<u8>) -> TPaNeOut,
    TPaNeOut: Future<Output = Option<Vec<u8>>>,
    TEGet: Fn(u64) -> babe::EpochInformation,
{
    // Start by verifying BABE.
    let babe_verify_success = {
        let start = babe::start_verify_header(babe::VerifyConfig {
            header: config.block_header.clone(),
            parent_block_header: config.parent_block_header,
            genesis_configuration: config.babe_genesis_configuration,
            now_from_unix_epoch: config.now_from_unix_epoch,
            block1_slot_number: config.block1_slot_number,
        })
        .map_err(Error::BabeVerification)?;

        match start {
            babe::SuccessOrPending::Pending(pending) => {
                let epoch_info = (config.babe_epoch_information_get)(pending.epoch_number());
                pending
                    .finish(&epoch_info)
                    .map_err(Error::BabeVerification)?
            }
            babe::SuccessOrPending::Success(success) => success,
        }
    };

    // BABE adds a seal at the end of the digest logs. This seal is guaranteed to be the last
    // item. We need to remove it before we can verify the unsealed header.
    let mut unsealed_header = config.block_header.clone();
    let _seal_log = unsealed_header.digest.pop().unwrap();

    let outcome = unsealed::verify_unsealed_block(unsealed::Config {
        parent_runtime: config.parent_runtime,
        block_header: unsealed_header,
        block_body: config.block_body,
        parent_storage_get: config.parent_storage_get,
        parent_storage_keys_prefix: config.parent_storage_keys_prefix,
        parent_storage_next_key: config.parent_storage_next_key,
        top_trie_root_calculation_cache: config.top_trie_root_calculation_cache,
    })
    .await
    .map_err(Error::Unsealed)?;

    Ok(Success {
        parent_runtime: outcome.parent_runtime,
        babe_epoch_change: babe_verify_success.epoch_change,
        slot_number: babe_verify_success.slot_number,
        storage_top_trie_changes: outcome.storage_top_trie_changes,
        top_trie_root_calculation_cache: outcome.top_trie_root_calculation_cache,
    })
}
