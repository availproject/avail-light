//! Unsealed block verification.
//!
//! After a block is produced by the runtime, a seal is applied on it. The seal contains a
//! signature, using a key known only to the author of the block, of the header of the block
//! without that seal.

use crate::{executor, header, trie::calculate_root};

use core::{cmp, convert::TryFrom as _, iter};
use futures::prelude::*;
use hashbrown::{HashMap, HashSet};

/// Configuration for an unsealed block verification.
// TODO: don't pass functions to the Config; instead, have a state-machine-like API
pub struct Config<'a, TBody, TPaAcc, TPaPref, TPaNe> {
    /// Runtime used to check the new block. Must be built using the `:code` of the parent
    /// block.
    pub runtime: executor::WasmVmPrototype,

    /// Header of the block to verify, in SCALE encoding.
    ///
    /// The `parent_hash` field is the hash of the parent whose storage can be accessed through
    /// the other fields.
    ///
    /// Block headers typically contain a `Seal` item as their last digest log item. When calling
    /// the [`verify_unsealed_block`] function, this header must **not** contain any `Seal` item.
    pub block_header: header::HeaderRef<'a>,

    /// Body of the block to verify.
    pub block_body: TBody,

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
    /// Runtime that was passed by [`Config`].
    pub parent_runtime: executor::WasmVmPrototype,
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
pub async fn verify_unsealed_block<
    'a,
    TBody,
    TExt,
    TPaAcc,
    TPaAccOut,
    TPaPref,
    TPaPrefOut,
    TPaNe,
    TPaNeOut,
>(
    mut config: Config<'a, TBody, TPaAcc, TPaPref, TPaNe>,
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
{
    let mut vm = config
        .runtime
        .run_vectored("Core_execute_block", {
            // The `Code_execute_block` function expects a SCALE-encoded `(header, body)`
            // where `body` is a `Vec<Vec<u8>>`. We perform the encoding manually to avoid
            // performing redundant data copies.

            // TODO: zero-cost
            let encoded_body_len = parity_scale_codec::Encode::encode(
                &parity_scale_codec::Compact(u32::try_from(config.block_body.len()).unwrap()),
            );

            let body = config.block_body.flat_map(|ext| {
                // TODO: don't allocate
                let encoded_ext_len = parity_scale_codec::Encode::encode(
                    &parity_scale_codec::Compact(u32::try_from(ext.as_ref().len()).unwrap()),
                );

                iter::once(either::Either::Left(encoded_ext_len))
                    .chain(iter::once(either::Either::Right(ext)))
            });

            config
                .block_header
                .scale_encoding()
                .map(|b| either::Either::Right(either::Either::Left(b)))
                .chain(iter::once(either::Either::Right(either::Either::Right(
                    encoded_body_len,
                ))))
                .chain(body.map(either::Either::Left))
        })
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
            executor::State::Finished(_) => {
                // TODO: assert output is empty?
                return Ok(Success {
                    parent_runtime: vm.into_prototype(),
                    storage_top_trie_changes: top_trie_changes,
                    top_trie_root_calculation_cache,
                });
            }
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
                    (config.parent_storage_get)(storage_key.to_vec()).await
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

                let mut current_value = if let Some(overlay) = top_trie_changes.get(storage_key) {
                    overlay.clone().unwrap_or(Vec::new())
                } else {
                    (config.parent_storage_get)(storage_key.to_vec())
                        .await
                        .unwrap_or(Vec::new())
                };

                append_to_storage_value(&mut current_value, value);
                top_trie_changes.insert(storage_key.to_vec(), Some(current_value));
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
                let mut calculation =
                    calculate_root::root_merkle_value(Some(&mut top_trie_root_calculation_cache));

                loop {
                    match calculation.next() {
                        calculate_root::Next::Finished(value) => {
                            resolve.finish_call(value);
                            break;
                        }
                        calculate_root::Next::AllKeys(keys) => {
                            let mut result = (config.parent_storage_keys_prefix)(Vec::new())
                                .now_or_never()
                                .unwrap()
                                .into_iter()
                                .filter(|v| top_trie_changes.get(v).map_or(true, |v| v.is_some()))
                                .collect::<HashSet<_>>();
                            // TODO: slow to iterate over everything?
                            for (key, value) in top_trie_changes.iter() {
                                if value.is_none() {
                                    continue;
                                }
                                result.insert(key.clone());
                            }
                            keys.inject(result.into_iter().map(|k| k.into_iter()))
                        }
                        calculate_root::Next::StorageValue(value_request) => {
                            let key = value_request.key().collect::<Vec<u8>>();
                            if let Some(overlay) = top_trie_changes.get(&key) {
                                value_request.inject(overlay.as_ref());
                            } else {
                                let val = (config.parent_storage_get)(key.to_vec())
                                    .now_or_never() // TODO: hack because `TrieRef` isn't async-friendly yet
                                    .unwrap();
                                value_request.inject(val.as_ref());
                            }
                        }
                    }
                }
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
                let vm_prototype = match executor::WasmVmPrototype::new(wasm_blob) {
                    Ok(w) => w,
                    Err(_) => {
                        resolve.finish_call(Err(()));
                        continue;
                    }
                };

                match executor::core_version(vm_prototype) {
                    Ok((version, _)) => {
                        resolve.finish_call(Ok(parity_scale_codec::Encode::encode(&version)));
                    }
                    Err(_) => {
                        resolve.finish_call(Err(()));
                    }
                }
            }
            s => unimplemented!("unimplemented externality: {:?}", s),
        }
    }
}

/// Performs the action described by [`executor::State::ExternalStorageAppend`] on an encoded
/// storage value.
fn append_to_storage_value(value: &mut Vec<u8>, to_add: &[u8]) {
    let curr_len = match <parity_scale_codec::Compact<u64> as parity_scale_codec::Decode>::decode(
        &mut &value[..],
    ) {
        Ok(l) => l,
        Err(_) => {
            value.clear();
            parity_scale_codec::Encode::encode_to(&parity_scale_codec::Compact(1u64), value);
            value.extend_from_slice(to_add);
            return;
        }
    };

    // Note: we use `checked_add`, as it is possible that the storage entry erroneously starts
    // with `u64::max_value()`.
    let new_len = match curr_len.0.checked_add(1) {
        Some(l) => parity_scale_codec::Compact(l),
        None => {
            value.clear();
            parity_scale_codec::Encode::encode_to(&parity_scale_codec::Compact(1u64), value);
            value.extend_from_slice(to_add);
            return;
        }
    };

    let curr_len_encoded_size =
        <parity_scale_codec::Compact<u64> as parity_scale_codec::CompactLen<u64>>::compact_len(
            &curr_len.0,
        );
    let new_len_encoded_size =
        <parity_scale_codec::Compact<u64> as parity_scale_codec::CompactLen<u64>>::compact_len(
            &new_len.0,
        );
    debug_assert!(
        new_len_encoded_size == curr_len_encoded_size
            || new_len_encoded_size == curr_len_encoded_size + 1
    );

    for _ in 0..(new_len_encoded_size - curr_len_encoded_size) {
        value.insert(0, 0);
    }

    parity_scale_codec::Encode::encode_to(&new_len, &mut (&mut value[..new_len_encoded_size]));
    value.extend_from_slice(to_add);
}
