//! Freestanding function that calculates the root of a radix-16 Merkle-Patricia trie.
//!
//! See the parent module documentation for an explanation of what the trie is.
//!
//! # Usage
//!
//! Example:
//!
//! ```
//! use std::collections::BTreeMap;
//! use substrate_lite::trie::calculate_root;
//!
//! // In this example, the storage consists in a binary tree map. Binary trees allow for an
//! // efficient implementation of `prefix_keys`.
//! let mut storage = BTreeMap::<Vec<u8>, Vec<u8>>::new();
//! storage.insert(b"foo".to_vec(), b"bar".to_vec());
//!
//! let trie_root = calculate_root::root_merkle_value(&storage, None);
//! assert_eq!(
//!     trie_root,
//!     [204, 86, 28, 213, 155, 206, 247, 145, 28, 169, 212, 146, 182, 159, 224, 82,
//!      116, 162, 143, 156, 19, 43, 183, 8, 41, 178, 204, 69, 41, 37, 224, 91]
//! );
//! ```
//!
//! You have the possibility to pass a [`CalculationCache`] to the calculation. This cache will
//! be filled with intermediary calculations and can later be passed again to calculate the root
//! in a more efficient way.
//!
//! When using a cache, be careful to properly invalidate cache entries whenever you perform
//! modifications on the trie associated to it.

use super::{
    nibble::{bytes_to_nibbles, Nibble},
    node_value, trie_structure,
};

use alloc::collections::BTreeMap;
use core::{convert::TryFrom as _, fmt, iter};

/// Trait passed to [`root_merkle_value`] that allows the function to access the content of the
/// trie.
// TODO: remove this trait and rewrite the calculation below as a coroutine-like state machine
pub trait TrieRef<'a>: Clone {
    type Key: AsRef<[u8]> + 'a;
    type Value: AsRef<[u8]> + 'a;
    type PrefixKeysIter: Iterator<Item = Self::Key> + 'a;

    /// Loads the value associated to the given key. Returns `None` if no value is present.
    ///
    /// Must always return the same value if called multiple times with the same key.
    fn get(self, key: &[u8]) -> Option<Self::Value>;

    /// Returns the list of keys with values that start with the given prefix.
    ///
    /// Must not return any duplicate.
    ///
    /// All the keys returned must start with the given prefix. It is an error to omit a key
    /// from the result.
    // TODO: `prefix` should have a lifetime too that ties it to the iterator, but I can't figure
    // out how to make this work
    fn prefix_keys(self, prefix: &[u8]) -> Self::PrefixKeysIter;
}

impl<'a> TrieRef<'a> for &'a BTreeMap<Vec<u8>, Vec<u8>> {
    type Key = &'a [u8];
    type Value = &'a [u8];
    type PrefixKeysIter = Box<dyn Iterator<Item = Self::Key> + 'a>;

    fn get(self, key: &[u8]) -> Option<Self::Value> {
        BTreeMap::get(self, key).map(|v| &v[..])
    }

    fn prefix_keys(self, prefix: &[u8]) -> Self::PrefixKeysIter {
        let prefix = prefix.to_vec(); // TODO: see comment about lifetime on the trait definition
        let iter = self
            .range({
                // We have to use a custom implementation of `std::ops::RangeBounds` because the
                // existing ones can't be used without passing a `Vec<u8>` by value.
                struct CustomRange<'a>(&'a [u8]);
                impl<'a> core::ops::RangeBounds<[u8]> for CustomRange<'a> {
                    fn start_bound(&self) -> core::ops::Bound<&[u8]> {
                        core::ops::Bound::Included(self.0)
                    }
                    fn end_bound(&self) -> core::ops::Bound<&[u8]> {
                        core::ops::Bound::Unbounded
                    }
                }
                CustomRange(&prefix)
            })
            .take_while(move |(k, _)| k.starts_with(&prefix))
            .map(|(k, _)| From::from(&k[..]));
        Box::new(iter)
    }
}

/// Cache containing intermediate calculation steps.
///
/// If the storage's content is modified, you **must** call the appropriate methods to invalidate
/// entries. Otherwise, the trie root calculation will yield an incorrect result.
pub struct CalculationCache {
    /// Structure of the trie.
    /// If `Some`, the structure is either fully conforming to the trie.
    structure: Option<trie_structure::TrieStructure<CacheEntry>>,
}

/// Custom data stored in each node in [`CalculationCache::structure`].
#[derive(Default)]
struct CacheEntry {
    merkle_value: Option<node_value::Output>,
}

impl CalculationCache {
    /// Builds a new empty cache.
    pub fn empty() -> Self {
        CalculationCache { structure: None }
    }

    /// Notify the cache that a storage value at the given key has been added, modified or removed.
    ///
    /// `has_value` must be true if there is now a storage value at the given key.
    pub fn storage_value_update(&mut self, key: &[u8], has_value: bool) {
        let structure = match &mut self.structure {
            Some(s) => s,
            None => return,
        };

        match (
            structure.node(bytes_to_nibbles(key.iter().cloned())),
            has_value,
        ) {
            (trie_structure::Entry::Vacant(entry), true) => {
                let inserted = entry
                    .insert_storage_value()
                    .insert(Default::default(), Default::default());

                // We have to invalidate the Merkle values of all the ancestors of the new node.
                let mut parent = inserted.into_parent();
                while let Some(mut p) = parent.take() {
                    p.user_data().merkle_value = None;
                    parent = p.into_parent();
                }
            }
            (trie_structure::Entry::Vacant(_), false) => {}
            (trie_structure::Entry::Occupied(mut entry), true) => {
                // Changing the storage value of a node changes its Merkle value and its ancestors'
                // Merkle values.
                entry.user_data().merkle_value = None;
                let mut parent = entry.into_parent();
                while let Some(mut p) = parent.take() {
                    p.user_data().merkle_value = None;
                    parent = p.into_parent();
                }
            }
            (trie_structure::Entry::Occupied(trie_structure::NodeAccess::Branch(_)), false) => {}
            (
                trie_structure::Entry::Occupied(trie_structure::NodeAccess::Storage(mut entry)),
                false,
            ) => {
                // All these situations are handled the same: we have to invalidate a certain
                // node's Merkle value and all of its ancestors' Merkle values.
                let mut node = match entry.remove() {
                    trie_structure::Remove::StorageToBranch(mut node) => {
                        trie_structure::NodeAccess::Branch(node)
                    }
                    trie_structure::Remove::BranchAlsoRemoved { mut sibling, .. } => sibling,
                    trie_structure::Remove::SingleRemoveChild { mut child, .. } => child,
                    trie_structure::Remove::SingleRemoveNoChild { mut parent, .. } => parent,
                    trie_structure::Remove::TrieNowEmpty { .. } => return,
                };

                node.user_data().merkle_value = None;
                let mut parent = node.into_parent();
                while let Some(mut p) = parent.take() {
                    p.user_data().merkle_value = None;
                    parent = p.into_parent();
                }
            }
        }
    }

    /// Notify the cache that all the storage values whose key start with the given prefix have
    /// been removed.
    pub fn prefix_remove_update(&mut self, prefix: &[u8]) {
        let structure = match &mut self.structure {
            Some(s) => s,
            None => return,
        };

        // TODO: implement correctly
        self.structure = None;

        /*
        if let Some(mut node) = structure.remove_prefix(bytes_to_nibbles(prefix).iter().cloned()) {
            node.user_data().merkle_value = None;
            let mut parent = node.into_parent();
            while let Some(mut p) = parent.take() {
                p.user_data().merkle_value = None;
                parent = p.into_parent();
            }
        } else if let Some(mut root_node) = structure.root_node() {
            root_node.user_data().merkle_value = None;
        }*/
    }
}

impl Default for CalculationCache {
    fn default() -> Self {
        Self::empty()
    }
}

impl fmt::Debug for CalculationCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The calculation cache is so large that printing its content is basically useless.
        f.debug_tuple("CalculationCache").finish()
    }
}

/// Calculates the Merkle value of the root node.
pub fn root_merkle_value<'a>(
    trie_access: impl TrieRef<'a>,
    mut cache: Option<&mut CalculationCache>,
) -> [u8; 32] {
    // The calculation that we perform relies on storing values in the cache and reloading them
    // afterwards. If the user didn't pass any cache, we create a temporary one.
    let mut temporary_cache = CalculationCache::empty();
    let cache_or_temporary = if let Some(cache) = cache {
        cache
    } else {
        &mut temporary_cache
    };

    fill_cache(trie_access, cache_or_temporary);

    // The `fill_cache` function guarantees to have filled the cache with the root node's
    // information.
    let mut root_node = cache_or_temporary.structure.as_mut().unwrap().root_node();
    if let Some(mut root_node) = root_node {
        root_node.user_data().merkle_value.clone().unwrap().into()
    } else {
        node_value::calculate_merke_root(node_value::Config {
            is_root: true,
            children: (0..16).map(|_| None),
            partial_key: iter::empty(),
            stored_value: None::<Vec<u8>>,
        })
        .into()
    }
}

/// Fills the cache given as parameter with at least the root node and the root node's merkle
/// value.
fn fill_cache<'a>(trie_access: impl TrieRef<'a>, mut cache: &mut CalculationCache) {
    // Make sure that `cache.structure` contains a trie structure that matches the trie.
    if cache.structure.is_none() {
        cache.structure = Some({
            let mut structure = trie_structure::TrieStructure::new();
            let keys = trie_access.clone().prefix_keys(&[]).collect::<Vec<_>>();
            for key in keys {
                structure
                    .node(bytes_to_nibbles(key.as_ref().iter().cloned()))
                    .into_vacant()
                    .unwrap()
                    .insert_storage_value()
                    .insert(Default::default(), Default::default());
            }
            structure
        });
    }

    // At this point `trie_structure` is guaranteed to match the trie, but its Merkle values might
    // be missing and need to be filled.
    let trie_structure = cache.structure.as_mut().unwrap();
    trie_structure.shrink_to_fit();

    // We now traverse the trie in attempt to find missing Merkle values.
    // We start with the root node. For each node, if its Merkle value is absent, we continue
    // iterating with its first child. If its Merkle value is present, we continue iterating with
    // the next sibling or, if it is the last sibling, the parent. In that situation where we jump
    // from last sibling to parent, we also calculate the parent's Merkle value in the process.
    // Due to this order of iteration, we traverse each node which lack a Merkle value twice, and
    // the Merkle value is calculated the second time.
    let mut current = match trie_structure.root_node() {
        Some(c) => c,
        None => return,
    };

    // `coming_from_child` is used to differentiate whether the previous iteration was the
    // previous sibling of `current` or the last child of `current`.
    let mut coming_from_child = false;

    loop {
        // If we already have a Merkle value, jump either to the next sibling (if any), or back
        // to the parent.
        if current.user_data().merkle_value.is_some() {
            debug_assert!(!coming_from_child);
            match current.into_next_sibling() {
                Ok(sibling) => {
                    current = sibling;
                    coming_from_child = false;
                    continue;
                }
                Err(curr) => {
                    if let Some(parent) = curr.into_parent() {
                        current = parent;
                        coming_from_child = true;
                        continue;
                    } else {
                        // No next sibling nor parent. We have finished traversing the tree.
                        return;
                    }
                }
            }
        }

        debug_assert!(current.user_data().merkle_value.is_none());

        // If previous iteration is from `current`'s previous sibling, we jump down to
        // `current`'s children.
        if !coming_from_child {
            match current.into_first_child() {
                Err(c) => current = c,
                Ok(first_child) => {
                    current = first_child;
                    coming_from_child = false;
                    continue;
                }
            }
        }

        // If we reach this, we are ready to calculate `current`'s Merkle value.

        // Load the stored value of this node.
        // TODO: do this in a better way, without allocating
        let node_key = current.full_key().collect::<Vec<_>>();
        let stored_value = if node_key.len() % 2 == 0 {
            let key_bytes = node_key
                .chunks(2)
                .map(|chunk| (u8::from(chunk[0]) << 4) | u8::from(chunk[1]))
                .collect::<Vec<_>>();

            trie_access
                .clone()
                .get(&key_bytes)
                .map(|v| v.as_ref().to_vec())
        } else {
            None
        };

        // Now calculate the Merkle value.
        let merkle_value = node_value::calculate_merke_root(node_value::Config {
            is_root: current.is_root_node(),
            children: (0..16u8).map(|child_idx| {
                if let Some(child) = current.child_user_data(Nibble::try_from(child_idx).unwrap()) {
                    Some(child.merkle_value.as_ref().unwrap())
                } else {
                    None
                }
            }),
            partial_key: current.partial_key(),
            stored_value,
        });

        current.user_data().merkle_value = Some(merkle_value);

        // We keep `current` as it is and iterate again. Since `merkle_value` is now `Some`, we
        // will jump to the next node.
        coming_from_child = false;
    }
}

// TODO: tests

// TODO: add a test that generates a random trie, calculates its root using a cache, modifies it
// randomly, invalidating the cache in the process, then calculates the root again, once with
// cache and once without cache, and compares the two values
