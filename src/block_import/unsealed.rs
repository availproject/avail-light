//! Unsealed block verification.
//!
//! After a block is produced by the runtime, a seal is applied on it. The seal contains a
//! signature, using a key known only to the author of the block, of the header of the block
//! without that seal.

use crate::{executor, header, trie::calculate_root};

use core::{cmp, convert::TryFrom as _, iter, slice};
use hashbrown::{HashMap, HashSet};

/// Configuration for an unsealed block verification.
pub struct Config<'a, TBody> {
    /// Runtime used to check the new block. Must be built using the `:code` of the parent
    /// block.
    pub parent_runtime: executor::WasmVmPrototype,

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

    /// Optional cache corresponding to the storage trie root hash calculation.
    pub top_trie_root_calculation_cache: Option<calculate_root::CalculationCache>,
}

/// Block successfully verified.
pub struct Success {
    /// Runtime that was passed by [`Config`].
    pub parent_runtime: executor::WasmVmPrototype,
    /// List of changes to the storage top trie that the block performs.
    pub storage_top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>>,
    /// List of changes to the offchain storage that this block performs.
    pub offchain_storage_changes: HashMap<Vec<u8>, Option<Vec<u8>>>,
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
pub fn verify_unsealed_block<'a>(
    config: Config<'a, impl ExactSizeIterator<Item = impl AsRef<[u8]> + Clone> + Clone>,
) -> Result<ReadyToRun, Error> {
    let vm = config
        .parent_runtime
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

    Ok(ReadyToRun {
        vm,
        top_trie_changes: Default::default(),
        offchain_storage_changes: Default::default(),
        top_trie_root_calculation_cache: Some(
            config.top_trie_root_calculation_cache.unwrap_or_default(),
        ),
        root_calculation: None,
    })
}

/// Current state of the verification.
#[must_use]
pub enum Verify {
    /// Verification is over.
    Finished(Result<Success, Error>),
    /// Verification is ready to continue.
    ReadyToRun(ReadyToRun),
    /// Loading a storage value is required in order to continue.
    StorageGet(StorageGet),
    /// Fetching the list of keys with a given prefix is required in order to continue.
    PrefixKeys(PrefixKeys),
    /// Fetching the key that follows a given one is required in order to continue.
    NextKey(NextKey),
}

/// Verification is ready to continue.
#[must_use]
pub struct ReadyToRun {
    vm: executor::WasmVm,

    /// Pending changes to the top storage trie that this block performs.
    top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>>,

    /// Pending changes to the offchain storage that this block performs.
    offchain_storage_changes: HashMap<Vec<u8>, Option<Vec<u8>>>,

    /// Cache passed by the user in the [`Config`]. Always `Some` except when we are currently
    /// calculating the trie state root.
    top_trie_root_calculation_cache: Option<calculate_root::CalculationCache>,

    /// Trie root calculation in progress.
    root_calculation: Option<calculate_root::RootMerkleValueCalculation>,
}

impl ReadyToRun {
    /// Continues the verification.
    pub fn run(mut self) -> Verify {
        loop {
            match self.vm.state() {
                executor::State::ReadyToRun(r) => r.run(),

                executor::State::Trapped => return Verify::Finished(Err(Error::Trapped)),
                executor::State::Finished(_) => {
                    // TODO: assert output is empty?
                    return Verify::Finished(Ok(Success {
                        parent_runtime: self.vm.into_prototype(),
                        storage_top_trie_changes: self.top_trie_changes,
                        offchain_storage_changes: self.offchain_storage_changes,
                        top_trie_root_calculation_cache: self
                            .top_trie_root_calculation_cache
                            .unwrap(),
                    }));
                }

                executor::State::ExternalStorageGet {
                    storage_key,
                    offset,
                    max_size,
                    resolve,
                } => {
                    if let Some(overlay) = self.top_trie_changes.get(storage_key) {
                        if let Some(overlay) = overlay {
                            // TODO: this clones the storage value, meh
                            let mut value = overlay.clone();
                            if usize::try_from(offset).unwrap() < value.len() {
                                value = value[usize::try_from(offset).unwrap()..].to_vec();
                                if usize::try_from(max_size).unwrap() < value.len() {
                                    value = value[..usize::try_from(max_size).unwrap()].to_vec();
                                }
                            } else {
                                value = Vec::new();
                            }
                            resolve.finish_call(Some(value));
                        } else {
                            resolve.finish_call(None);
                        }
                    } else {
                        return Verify::StorageGet(StorageGet { inner: self });
                    }
                }

                executor::State::ExternalStorageSet {
                    storage_key,
                    new_storage_value,
                    resolve,
                } => {
                    self.top_trie_root_calculation_cache
                        .as_mut()
                        .unwrap()
                        .storage_value_update(storage_key, new_storage_value.is_some());
                    self.top_trie_changes
                        .insert(storage_key.to_vec(), new_storage_value.map(|v| v.to_vec()));
                    resolve.finish_call(());
                }

                executor::State::ExternalStorageAppend {
                    storage_key,
                    value,
                    resolve,
                } => {
                    self.top_trie_root_calculation_cache
                        .as_mut()
                        .unwrap()
                        .storage_value_update(storage_key, true);

                    if let Some(current_value) = self.top_trie_changes.get(storage_key) {
                        let mut current_value = current_value.clone().unwrap_or_default();
                        append_to_storage_value(&mut current_value, value);
                        self.top_trie_changes
                            .insert(storage_key.to_vec(), Some(current_value));
                        resolve.finish_call(());
                    } else {
                        return Verify::StorageGet(StorageGet { inner: self });
                    }
                }

                executor::State::ExternalStorageClearPrefix {
                    storage_key,
                    resolve,
                } => {
                    return Verify::PrefixKeys(PrefixKeys { inner: self });
                }

                executor::State::ExternalStorageRoot { resolve } => {
                    if self.root_calculation.is_none() {
                        self.root_calculation = Some(calculate_root::root_merkle_value(Some(
                            self.top_trie_root_calculation_cache.take().unwrap(),
                        )));
                    }

                    match self.root_calculation.take().unwrap() {
                        calculate_root::RootMerkleValueCalculation::Finished { hash, cache } => {
                            self.top_trie_root_calculation_cache = Some(cache);
                            resolve.finish_call(hash);
                        }
                        calculate_root::RootMerkleValueCalculation::AllKeys(keys) => {
                            self.root_calculation =
                                Some(calculate_root::RootMerkleValueCalculation::AllKeys(keys));
                            return Verify::PrefixKeys(PrefixKeys { inner: self });
                        }
                        calculate_root::RootMerkleValueCalculation::StorageValue(value_request) => {
                            // TODO: allocating a Vec, meh
                            if let Some(overlay) = self
                                .top_trie_changes
                                .get(&value_request.key().collect::<Vec<_>>())
                            {
                                self.root_calculation =
                                    Some(value_request.inject(overlay.as_ref()));
                            } else {
                                self.root_calculation =
                                    Some(calculate_root::RootMerkleValueCalculation::StorageValue(
                                        value_request,
                                    ));
                                return Verify::StorageGet(StorageGet { inner: self });
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
                } => return Verify::NextKey(NextKey { inner: self }),

                executor::State::ExternalOffchainStorageSet {
                    storage_key,
                    new_storage_value,
                    resolve,
                } => {
                    self.offchain_storage_changes
                        .insert(storage_key.to_vec(), new_storage_value.map(|v| v.to_vec()));
                    resolve.finish_call(());
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
}

/// Loading a storage value is required in order to continue.
#[must_use]
pub struct StorageGet {
    inner: ReadyToRun,
}

impl StorageGet {
    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    // TODO: shouldn't be mut
    pub fn key<'a>(&'a mut self) -> impl Iterator<Item = impl AsRef<[u8]> + 'a> + 'a {
        match self.inner.vm.state() {
            executor::State::ExternalStorageGet { storage_key, .. }
            | executor::State::ExternalStorageAppend { storage_key, .. } => {
                either::Either::Left(iter::once(either::Either::Left(storage_key)))
            }

            executor::State::ExternalStorageRoot { .. } => {
                if let calculate_root::RootMerkleValueCalculation::StorageValue(value_request) =
                    self.inner.root_calculation.as_ref().unwrap()
                {
                    struct One(u8);
                    impl AsRef<[u8]> for One {
                        fn as_ref(&self) -> &[u8] {
                            slice::from_ref(&self.0)
                        }
                    }
                    either::Either::Right(value_request.key().map(One).map(either::Either::Right))
                } else {
                    // We only create a `StorageGet` if the state is `StorageValue`.
                    panic!()
                }
            }

            // We only create a `StorageGet` if the state is one of the above.
            _ => unreachable!(),
        }
    }

    /// Injects the corresponding storage value.
    // TODO: `value` parameter should be something like `Iterator<Item = impl AsRef<[u8]>`
    pub fn inject_value(mut self, value: Option<&[u8]>) -> ReadyToRun {
        match self.inner.vm.state() {
            executor::State::ExternalStorageGet {
                storage_key,
                offset,
                max_size,
                resolve,
            } => {
                if let Some(mut value) = value {
                    if usize::try_from(offset).unwrap() < value.len() {
                        value = &value[usize::try_from(offset).unwrap()..];
                        if usize::try_from(max_size).unwrap() < value.len() {
                            value = &value[..usize::try_from(max_size).unwrap()];
                        }
                    }

                    resolve.finish_call(Some(value.to_vec())); // TODO: overhead
                } else {
                    resolve.finish_call(None);
                }
            }
            executor::State::ExternalStorageAppend {
                storage_key,
                value: to_add,
                resolve,
            } => {
                let mut value = value.map(|v| v.to_vec()).unwrap_or_default();
                // TODO: could be less overhead?
                append_to_storage_value(&mut value, to_add);
                self.inner
                    .top_trie_changes
                    .insert(storage_key.to_vec(), Some(value.clone()));
                resolve.finish_call(());
            }
            executor::State::ExternalStorageRoot { .. } => {
                if let calculate_root::RootMerkleValueCalculation::StorageValue(value_request) =
                    self.inner.root_calculation.take().unwrap()
                {
                    self.inner.root_calculation = Some(value_request.inject(value));
                } else {
                    // We only create a `StorageGet` if the state is `StorageValue`.
                    panic!()
                }
            }

            // We only create a `StorageGet` if the state is one of the above.
            _ => unreachable!(),
        };

        self.inner
    }
}

/// Fetching the list of keys with a given prefix is required in order to continue.
#[must_use]
pub struct PrefixKeys {
    inner: ReadyToRun,
}

impl PrefixKeys {
    /// Returns the prefix whose keys to load.
    // TODO: don't take &mut mut but &self
    pub fn prefix(&mut self) -> &[u8] {
        match self.inner.vm.state() {
            executor::State::ExternalStorageClearPrefix { storage_key, .. } => storage_key,
            executor::State::ExternalStorageRoot { .. } => &[],

            // We only create a `PrefixKeys` if the state is one of the above.
            _ => unreachable!(),
        }
    }

    /// Injects the list of keys.
    pub fn inject_keys(mut self, keys: impl Iterator<Item = impl AsRef<[u8]>>) -> ReadyToRun {
        match self.inner.vm.state() {
            executor::State::ExternalStorageClearPrefix {
                storage_key,
                resolve,
            } => {
                // TODO: use prefix_remove_update once optimized
                //top_trie_root_calculation_cache.prefix_remove_update(storage_key);

                for key in keys {
                    self.inner
                        .top_trie_root_calculation_cache
                        .as_mut()
                        .unwrap()
                        .storage_value_update(key.as_ref(), false);
                    self.inner
                        .top_trie_changes
                        .insert(key.as_ref().to_vec(), None);
                }
                for (key, value) in self.inner.top_trie_changes.iter_mut() {
                    if !key.starts_with(&storage_key) {
                        continue;
                    }
                    if value.is_none() {
                        continue;
                    }
                    self.inner
                        .top_trie_root_calculation_cache
                        .as_mut()
                        .unwrap()
                        .storage_value_update(key, false);
                    *value = None;
                }
                resolve.finish_call(());
            }

            executor::State::ExternalStorageRoot { .. } => {
                if let calculate_root::RootMerkleValueCalculation::AllKeys(all_keys) =
                    self.inner.root_calculation.take().unwrap()
                {
                    // TODO: overhead
                    let mut list = keys
                        .filter(|v| {
                            self.inner
                                .top_trie_changes
                                .get(v.as_ref())
                                .map_or(true, |v| v.is_some())
                        })
                        .map(|v| v.as_ref().to_vec())
                        .collect::<HashSet<_>>();
                    // TODO: slow to iterate over everything?
                    for (key, value) in self.inner.top_trie_changes.iter() {
                        if value.is_none() {
                            continue;
                        }
                        list.insert(key.clone());
                    }
                    self.inner.root_calculation =
                        Some(all_keys.inject(list.into_iter().map(|k| k.into_iter())));
                } else {
                    // We only create a `PrefixKeys` if the state is `AllKeys`.
                    panic!()
                }
            }

            // We only create a `PrefixKeys` if the state is one of the above.
            _ => unreachable!(),
        };

        self.inner
    }
}

/// Fetching the key that follows a given one is required in order to continue.
#[must_use]
pub struct NextKey {
    inner: ReadyToRun,
}

impl NextKey {
    /// Returns the key whose next key must be passed back.
    // TODO: don't take &mut mut but &self
    pub fn key(&mut self) -> &[u8] {
        match self.inner.vm.state() {
            executor::State::ExternalStorageNextKey { storage_key, .. } => storage_key,
            _ => unreachable!(),
        }
    }

    /// Injects the key.
    pub fn inject_key(mut self, key: Option<impl AsRef<[u8]>>) -> ReadyToRun {
        match self.inner.vm.state() {
            executor::State::ExternalStorageNextKey {
                storage_key,
                resolve,
            } => {
                // The next key can be either the one passed by the user or one key in the current
                // pending storage changes that has been inserted during the verification.
                // TODO: not optimized regarding cloning
                // TODO: also not optimized in terms of searching time ; should really be a BTreeMap or something
                let in_overlay = self
                    .inner
                    .top_trie_changes
                    .keys()
                    .filter(|k| &***k > storage_key)
                    .min();
                // TODO: `to_vec()` overhead
                let outcome = match (key, in_overlay) {
                    (Some(a), Some(b)) => Some(cmp::min(a.as_ref().to_vec(), b.clone())),
                    (Some(a), None) => Some(a.as_ref().to_vec()),
                    (None, Some(b)) => Some(b.clone()),
                    (None, None) => None,
                };
                resolve.finish_call(outcome);
            }

            // We only create a `NextKey` if the state is one of the above.
            _ => unreachable!(),
        };

        self.inner
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
