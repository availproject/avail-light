//! Block verification.
//!
//! When we receive a block from the network whose number is higher than the number of the best
//! block that we know of, we can potentially add this block to the chain and treat it as the new
//! best block.
//!
//! But before doing so, we must first verify whether the block is correct. In other words, that
//! all the extrinsics in the block can indeed be applied on top of its parent.

use crate::{block, executor, storage, trie::calculate_root};

use alloc::sync::Arc;
use core::{cmp, convert::TryFrom as _, pin::Pin};
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};
use hashbrown::HashMap;
use primitive_types::H256;

/// Configuration for a block verification.
pub struct Config<'a, TPaAcc, TPaPref, TPaNe> {
    /// Runtime used to check the new block. Must be built using the `:code` of the parent
    /// block.
    pub runtime: &'a executor::WasmBlob,

    /// Header of the block to verify.
    ///
    /// The `parent_hash` field is the hash of the parent whose storage can be accessed through
    /// the other fields.
    // TODO: weaker typing
    pub block_header: &'a crate::block::Header,

    /// Body of the block to verify.
    pub block_body: &'a [crate::block::Extrinsic],

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

    /// Optional cache corresponding to the storage trie root hash calculation.
    pub top_trie_root_calculation_cache: Option<calculate_root::CalculationCache>,
}

/// Block successfully verified.
pub struct Success {
    /// List of changes to the storage top trie that the block performs.
    pub storage_top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>>,
    /// Cache used for calculating the top trie root.
    pub top_trie_root_calculation_cache: calculate_root::CalculationCache,
    // TOOD: logs written by the runtime
}

/// Error that can happen during the verification.
#[derive(Debug, Clone, derive_more::Display)]
pub enum Error {
    /// Error while executing the Wasm virtual machine.
    // TODO: add the logs written by the runtime
    Trapped,
}

/// Verifies whether a block is valid.
// TODO: Aura/BABE
pub async fn verify_block<'a, TPaAcc, TPaAccOut, TPaPref, TPaPrefOut, TPaNe, TPaNeOut>(
    config: Config<'a, TPaAcc, TPaPref, TPaNe>,
) -> Result<Success, Error>
where
    // TODO: ugh, we pass Vecs because of lifetime clusterfuck
    TPaAcc: Fn(Vec<u8>) -> TPaAccOut,
    TPaAccOut: Future<Output = Option<Vec<u8>>>,
    TPaPref: Fn(Vec<u8>) -> TPaPrefOut,
    TPaPrefOut: Future<Output = Vec<Vec<u8>>>,
    TPaNe: Fn(Vec<u8>) -> TPaNeOut,
    TPaNeOut: Future<Output = Option<Vec<u8>>>,
{
    // Remove the seal
    // TODO: obviously make this less hacky
    // TODO: BABE appends a seal to each block that the runtime doesn't know about
    let mut block_header = config.block_header.clone();
    let mut seal_log = block_header.digest.logs.pop().unwrap();

    let mut vm = executor::WasmVm::new(
        config.runtime,
        executor::FunctionToCall::CoreExecuteBlock(&crate::block::Block {
            header: block_header,
            extrinsics: config.block_body.to_vec(),
        }),
    )
    .unwrap();

    // Pending changes to the top storage trie that this block performs.
    let mut top_trie_changes = HashMap::<Vec<u8>, Option<Vec<u8>>>::new();
    // Cache passed as part of the configuration. Initially guaranteed to match the storage or the
    // parent, then updated to match the changes made during the verification.
    let mut top_trie_root_calculation_cache =
        config.top_trie_root_calculation_cache.unwrap_or_default();

    loop {
        match vm.state() {
            executor::State::ReadyToRun(r) => r.run(),
            executor::State::Finished(executor::Success::CoreExecuteBlock) => {
                return Ok(Success {
                    storage_top_trie_changes: top_trie_changes,
                    top_trie_root_calculation_cache,
                })
            }
            executor::State::Finished(_) => unreachable!(),
            executor::State::Trapped => return Err(Error::Trapped),

            executor::State::ExternalStorageGet {
                storage_key,
                offset,
                max_size,
                resolve,
            } => {
                // TODO: this clones the storage value, meh
                let mut value = if let Some(overlay) = top_trie_changes.get(storage_key) {
                    overlay.clone()
                } else {
                    (config.parent_storage_get)(storage_key.to_vec())
                        .await
                        .map(|v| v.to_vec())
                };
                if let Some(value) = &mut value {
                    if usize::try_from(offset).unwrap() < value.len() {
                        *value = value[usize::try_from(offset).unwrap()..].to_vec();
                        if usize::try_from(max_size).unwrap() < value.len() {
                            *value = value[..usize::try_from(max_size).unwrap()].to_vec();
                        }
                    } else {
                        *value = Vec::new();
                    }
                }
                resolve.finish_call(value);
            }
            executor::State::ExternalStorageSet {
                storage_key,
                new_storage_value,
                resolve,
            } => {
                top_trie_root_calculation_cache.invalidate_node(storage_key);
                top_trie_changes
                    .insert(storage_key.to_vec(), new_storage_value.map(|v| v.to_vec()));
                resolve.finish_call(());
            }
            executor::State::ExternalStorageAppend {
                storage_key,
                value,
                resolve,
            } => {
                top_trie_root_calculation_cache.invalidate_node(storage_key);

                let mut current_value = if let Some(overlay) = top_trie_changes.get(storage_key) {
                    overlay.clone().unwrap_or(Vec::new())
                } else {
                    (config.parent_storage_get)(storage_key.to_vec())
                        .await
                        .map(|v| v.to_vec())
                        .unwrap_or(Vec::new())
                };
                let curr_len =
                    <parity_scale_codec::Compact<u64> as parity_scale_codec::Decode>::decode(
                        &mut &current_value[..],
                    );
                let new_value = if let Ok(mut curr_len) = curr_len {
                    let len_size = <parity_scale_codec::Compact::<u64> as parity_scale_codec::CompactLen::<u64>>::compact_len(&curr_len.0);
                    curr_len.0 += 1;
                    let mut new_value = parity_scale_codec::Encode::encode(&curr_len);
                    new_value.extend_from_slice(&current_value[len_size..]);
                    new_value.extend_from_slice(value);
                    new_value
                } else {
                    let mut new_value =
                        parity_scale_codec::Encode::encode(&parity_scale_codec::Compact(1u64));
                    new_value.extend_from_slice(value);
                    new_value
                };
                top_trie_changes.insert(storage_key.to_vec(), Some(new_value));
                resolve.finish_call(());
            }
            executor::State::ExternalStorageClearPrefix {
                storage_key,
                resolve,
            } => {
                top_trie_root_calculation_cache.invalidate_prefix(storage_key);

                for key in (config.parent_storage_keys_prefix)(Vec::new()).await {
                    if !key.starts_with(&storage_key) {
                        continue;
                    }
                    top_trie_changes.insert(key.to_vec(), None);
                }
                for (key, value) in top_trie_changes.iter_mut() {
                    if !key.starts_with(&storage_key) {
                        continue;
                    }
                    *value = None;
                }
                resolve.finish_call(());
            }
            executor::State::ExternalStorageRoot { resolve } => {
                let mut trie = crate::trie::Trie::new();
                for key in (config.parent_storage_keys_prefix)(Vec::new()).await {
                    let value = (config.parent_storage_get)(key.to_vec())
                        .await
                        .unwrap()
                        .to_vec();
                    trie.insert(key.as_ref(), value);
                }
                for (key, value) in top_trie_changes.iter() {
                    if let Some(value) = value.as_ref() {
                        trie.insert(key, value.clone())
                    } else {
                        trie.remove(key);
                    }
                }
                let hash = trie.root_merkle_value(Some(&mut top_trie_root_calculation_cache));
                resolve.finish_call(hash);
            }
            executor::State::ExternalStorageChangesRoot {
                parent_hash,
                resolve,
            } => {
                // TODO: this is probably one of the most complicated things to
                // implement, but slava told me that it's ok to just return None on
                // flaming fir because the feature is disabled
                resolve.finish_call(None);
            }
            executor::State::ExternalStorageNextKey {
                storage_key,
                resolve,
            } => {
                // TODO: not optimized regarding cloning
                let in_storage = (config.parent_storage_next_key)(storage_key.to_vec())
                    .await
                    .map(|v| v.to_vec());
                let in_overlay = top_trie_changes
                    .keys()
                    .filter(|k| &***k > storage_key)
                    .min();
                let outcome = match (in_storage, in_overlay) {
                    (Some(a), Some(b)) => Some(cmp::min(a, b.clone())),
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b.clone()),
                    (None, None) => None,
                };
                resolve.finish_call(outcome);
            }
            s => unimplemented!("unimplemented externality: {:?}", s),
        }
    }
}
