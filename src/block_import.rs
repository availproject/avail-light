//! Block verification.
//!
//! When we receive a block from the network whose number is higher than the number of the best
//! block that we know of, we can potentially add this block to the chain and treat it as the new
//! best block.
//!
//! But before doing so, we must first verify whether the block is correct. In other words, that
//! all the extrinsics in the block can indeed be applied on top of its parent.

use crate::{executor, trie::calculate_root};

use alloc::borrow::Cow;
use core::{cmp, convert::TryFrom as _};
use futures::prelude::*;
use hashbrown::{HashMap, HashSet};

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
    mut config: Config<'a, TPaAcc, TPaPref, TPaNe>,
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
    let _seal_log = block_header.digest.logs.pop().unwrap();

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
    // TODO: we use `take()` as a work-around because we use `config` below, but it could be fixed
    let mut top_trie_root_calculation_cache = config
        .top_trie_root_calculation_cache
        .take()
        .unwrap_or_default();

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
                top_trie_root_calculation_cache
                    .storage_value_update(storage_key, new_storage_value.is_some());
                top_trie_changes
                    .insert(storage_key.to_vec(), new_storage_value.map(|v| v.to_vec()));
                resolve.finish_call(());
            }
            executor::State::ExternalStorageAppend {
                storage_key,
                value,
                resolve,
            } => {
                top_trie_root_calculation_cache.storage_value_update(storage_key, true);

                let current_value = if let Some(overlay) = top_trie_changes.get(storage_key) {
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
                // TODO: use prefix_remove_update once optimized
                //top_trie_root_calculation_cache.prefix_remove_update(storage_key);

                for key in (config.parent_storage_keys_prefix)(Vec::new()).await {
                    if !key.starts_with(&storage_key) {
                        continue;
                    }
                    top_trie_root_calculation_cache.storage_value_update(&key, false);
                    top_trie_changes.insert(key, None);
                }
                for (key, value) in top_trie_changes.iter_mut() {
                    if !key.starts_with(&storage_key) {
                        continue;
                    }
                    top_trie_root_calculation_cache.storage_value_update(key, false);
                    *value = None;
                }
                resolve.finish_call(());
            }
            executor::State::ExternalStorageRoot { resolve } => {
                // TODO: clean this code so that it's more readable
                struct TrieAccess<'a, TPaAcc, TPaPref, TPaNe> {
                    config: &'a Config<'a, TPaAcc, TPaPref, TPaNe>,
                    overlay_changes: &'a HashMap<Vec<u8>, Option<Vec<u8>>>,
                }

                impl<'a, TPaAcc, TPaPref, TPaNe> Copy for TrieAccess<'a, TPaAcc, TPaPref, TPaNe> {}
                impl<'a, TPaAcc, TPaPref, TPaNe> Clone for TrieAccess<'a, TPaAcc, TPaPref, TPaNe> {
                    fn clone(&self) -> Self {
                        Self {
                            config: self.config,
                            overlay_changes: self.overlay_changes,
                        }
                    }
                }

                impl<'a, TPaAcc, TPaAccOut, TPaPref, TPaPrefOut, TPaNe, TPaNeOut>
                    calculate_root::TrieRef<'a> for TrieAccess<'a, TPaAcc, TPaPref, TPaNe>
                where
                    // TODO: ugh, we pass Vecs because of lifetime clusterfuck
                    TPaAcc: Fn(Vec<u8>) -> TPaAccOut,
                    TPaAccOut: Future<Output = Option<Vec<u8>>>,
                    TPaPref: Fn(Vec<u8>) -> TPaPrefOut,
                    TPaPrefOut: Future<Output = Vec<Vec<u8>>>,
                    TPaNe: Fn(Vec<u8>) -> TPaNeOut,
                    TPaNeOut: Future<Output = Option<Vec<u8>>>,
                {
                    type Key = Vec<u8>;
                    type Value = Cow<'a, [u8]>;
                    type PrefixKeysIter = Box<dyn Iterator<Item = Self::Key> + 'a>;

                    /// Loads the value associated to the given key. Returns `None` if no value is present.
                    ///
                    /// Must always return the same value if called multiple times with the same key.
                    fn get(self, key: &[u8]) -> Option<Self::Value> {
                        if let Some(overlay) = self.overlay_changes.get(key) {
                            overlay.as_ref().map(|v| Cow::Borrowed(&v[..]))
                        } else {
                            (self.config.parent_storage_get)(key.to_vec())
                                .now_or_never() // TODO: hack because `TrieRef` isn't async-friendly yet
                                .unwrap()
                                .map(|v| Cow::Owned(v))
                        }
                    }

                    fn prefix_keys(self, prefix: &[u8]) -> Self::PrefixKeysIter {
                        // TODO: optimize
                        let mut result = (self.config.parent_storage_keys_prefix)(prefix.to_vec())
                            .now_or_never()
                            .unwrap()
                            .into_iter()
                            .filter(|v| self.overlay_changes.get(v).map_or(true, |v| v.is_some()))
                            .collect::<HashSet<_>>();
                        // TODO: slow to iterate over everything?
                        for (key, value) in self.overlay_changes.iter() {
                            if value.is_none() || !key.starts_with(&prefix) {
                                continue;
                            }
                            result.insert(key.clone());
                        }
                        Box::new(result.into_iter())
                    }
                }

                let hash = calculate_root::root_merkle_value(
                    TrieAccess {
                        config: &config,
                        overlay_changes: &top_trie_changes,
                    },
                    Some(&mut top_trie_root_calculation_cache),
                );
                resolve.finish_call(hash);
            }
            executor::State::ExternalStorageChangesRoot {
                parent_hash: _,
                resolve,
            } => {
                // TODO: this is probably one of the most complicated things to implement
                // TODO: must return None iff `state_at(parent_block).exists_storage(&well_known_keys::CHANGES_TRIE_CONFIG).unwrap() == false`
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

            executor::State::CallRuntimeVersion { wasm_blob, resolve } => {
                // TODO: is there maybe a better way to handle that?
                let wasm_blob = match executor::WasmBlob::from_bytes(wasm_blob) {
                    Ok(w) => w,
                    Err(_) => {
                        resolve.finish_call(Err(()));
                        continue;
                    }
                };
                let mut inner_vm = match executor::WasmVm::new(
                    &wasm_blob,
                    executor::FunctionToCall::CoreVersion,
                ) {
                    Ok(v) => v,
                    Err(_) => {
                        resolve.finish_call(Err(()));
                        continue;
                    }
                };

                let outcome = loop {
                    match inner_vm.state() {
                        executor::State::ReadyToRun(r) => r.run(),
                        executor::State::Finished(executor::Success::CoreVersion(version)) => {
                            break Ok(parity_scale_codec::Encode::encode(&version));
                        }
                        executor::State::Finished(_) => unreachable!(),
                        executor::State::Trapped => break Err(()),

                        // Since there are potential ambiguities we don't allow any storage access
                        // or anything similar. The last thing we want is to have an infinite
                        // recursion of runtime calls.
                        _ => break Err(()),
                    }
                };

                resolve.finish_call(outcome);
            }
            s => unimplemented!("unimplemented externality: {:?}", s),
        }
    }
}
