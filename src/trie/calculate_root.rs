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

use alloc::{
    borrow::Cow,
    collections::{btree_map::Entry, BTreeMap},
};
use core::{convert::TryFrom as _, fmt};

use parity_scale_codec::Encode as _;

/// Trait passed to [`root_merkle_value`] that allows the function to access the content of the
/// trie.
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
#[derive(Debug)]
pub struct CalculationCache {
    /// Root node of the trie, if known.
    root_node: Option<TrieNodeKey>,

    /// Cache of node values of known nodes.
    entries: BTreeMap<TrieNodeKey, CacheEntry>,
}

/// Entry in the [`CalculationCache`].
#[derive(Debug, Clone)]
struct CacheEntry {
    /// Merkle value of this node. `None` if it needs to be recalculated.
    ///
    /// When the user invalidates a specific storage value, we only need to refresh the merkle
    /// value of the corresponding node while the parent and children remain the same.
    merkle_value: Option<Vec<u8>>,
    /// If this is the root node, length of the key. Otherwise, number of nibbles between this node
    /// and its parent, minus one.
    partial_key_length: u32,
    /// How to reach the chidren of this node.
    children: Vec<(Nibble, TrieNodeKey)>,
}

impl CalculationCache {
    /// Builds a new empty cache.
    pub fn empty() -> Self {
        CalculationCache {
            root_node: None,
            entries: BTreeMap::new(),
        }
    }

    /// Notify the cache that the value at the given key has been modified.
    ///
    /// > **Note**: If the value has been entirely removed, you must call
    // >            [`CalculationCache::invalidate_node`] instead.
    pub fn invalidate_storage_value(&mut self, key: &[u8]) {
        // We invalidate the `merkle_value` of `key` and `key`'s ancestors.
        let mut next_to_invalidate = Some(TrieNodeKey::from_bytes(key));

        while let Some(mut to_invalidate) = next_to_invalidate.take() {
            if let Some(entry) = self.entries.get_mut(&to_invalidate) {
                entry.merkle_value = None;

                let parent_key_len = to_invalidate.nibbles.len()
                    - usize::try_from(entry.partial_key_length).unwrap();
                to_invalidate.nibbles.truncate(parent_key_len);
                next_to_invalidate = Some(to_invalidate);
            }
        }
    }

    /// Notify the cache that the value at the given key has been modified or has been removed.
    pub fn invalidate_node(&mut self, key: &[u8]) {
        // Considering the the node value of the direct children of `key` depends on the location
        // of their parent, we have to invalidate them as well. We just take a shortcut and use
        // `invalidate_prefix`.
        self.invalidate_prefix(key);
    }

    /// Notify the cache that all the values whose key starts with the given prefix have been
    /// modified or have been removed.
    pub fn invalidate_prefix(&mut self, _prefix: &[u8]) {
        // TODO: actually implement
        self.entries.clear();
    }
}

impl Default for CalculationCache {
    fn default() -> Self {
        Self::empty()
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

    // The `fill_cache` function guarantees to have filled the cache with at least the root node's
    // information.
    let root_node = cache_or_temporary.root_node.as_ref().unwrap();
    let root_merkle = cache_or_temporary
        .entries
        .get(root_node)
        .unwrap()
        .merkle_value
        .as_ref()
        .unwrap();

    // The root node's merkle value is guaranteed to be a hash, and therefore exactly 32 bytes.
    let mut out = [0; 32];
    out.copy_from_slice(&root_merkle);
    out
}

/// Fills the cache given as parameter with at least the root node and the root node's merkle
/// value.
fn fill_cache<'a>(trie_access: impl TrieRef<'a>, mut cache: &mut CalculationCache) {
    // Start by figuring out the key of the root node, possibly with the help of the cache.
    // The root node is not necessarily the one with an empty key. Just like any other node,
    // the root might have been merged with its lone children.
    let root_node = if let Some(root_node) = &cache.root_node {
        root_node.clone()
    } else {
        // Recomputing this is very expensive, since we enumerate every single key in the
        // storage.
        // TODO: clusterfuck of lifetimes
        let list = trie_access.clone().prefix_keys(&[]).collect::<Vec<_>>();
        let root_node = common_prefix(list.iter().map(|k| k.as_ref())).unwrap_or(TrieNodeKey {
            nibbles: Vec::new(),
        });
        cache.root_node = Some(root_node.clone());
        root_node
    };

    // The code below pops the last element of this queue, fills it in the cache, and optionally
    // pushes new element to the queue. When the queue is empty, the function returns.
    //
    // Each element in the queue is a tuple with:
    // - The key of the immediate parent of the node to process.
    // - The next nibble from the parent towards the direction of the node to process. `None` if
    //   the node to process is the root node.
    // - The partial key of the node to process.
    let mut queue_to_process: Vec<(TrieNodeKey, Option<Nibble>, TrieNodeKey)> =
        vec![(TrieNodeKey::empty(), None, root_node)];

    loop {
        let (parent_key, child_index, partial_key) = match queue_to_process.pop() {
            Some(p) => p,
            None => return,
        };

        // Complete key of the node to process. Combines the three components we just poped.
        let combined_key = {
            let mut combined_key = parent_key.clone();
            if let Some(child_index) = &child_index {
                combined_key.nibbles.push(child_index.clone());
            }
            combined_key.nibbles.extend(partial_key.nibbles.clone());
            combined_key
        };

        // Grab the existing cache entry, or insert a new one if necessary.
        let cache_entry_copy = match cache.entries.entry(combined_key.clone()) {
            Entry::Occupied(e) => {
                // If the merkle value is already in the cache, then our job is done and we can
                // process the next node.
                if e.get().merkle_value.is_some() {
                    continue;
                }
                e.get().clone()
            }
            Entry::Vacant(e) => e
                .insert(CacheEntry {
                    merkle_value: None,
                    partial_key_length: u32::try_from(partial_key.nibbles.len()).unwrap(),
                    children: child_nodes(trie_access.clone(), &combined_key).collect(),
                })
                .clone(),
        };

        // Build the `children_bitmap`, which is a `u16` where each bit is set if there exists
        // a child there, and `children_merkle_value_concat`, the concatenation of the merkle
        // values of all of our children.
        //
        // We do this first, because we might have to interrupt the processing of this node if
        // one of the children doesn't have a cache entry.
        let (children_bitmap, children_merkle_value_concat) = {
            // TODO: unstable, see https://github.com/rust-lang/rust/issues/53485
            //debug_assert!(cache_entry.children.iter().map(|(n, _)| n).is_sorted());

            // TODO: shouldn't we first check whether all the children are in cache to speed
            // things up and avoid extra copies? should benchmark this

            // TODO: review this code below for unnecessary allocations

            let mut children_bitmap = 0u16;
            let mut children_merkle_value_concat =
                Vec::with_capacity(cache_entry_copy.children.len() * 33);

            // Will be set to the children whose merkle value of a child is missing from the cache.
            let mut missing_children = Vec::with_capacity(16);

            for (child_index, child_partial_key) in &cache_entry_copy.children {
                children_bitmap |= 1 << u32::from(child_index.0);

                let child_combined_key = {
                    let mut k = combined_key.clone();
                    k.nibbles.push(child_index.clone());
                    k.nibbles.extend(child_partial_key.nibbles.iter().cloned());
                    k
                };

                if let Some(merkle_value) = cache
                    .entries
                    .get(&child_combined_key)
                    .and_then(|e| e.merkle_value.as_ref())
                {
                    // TODO: this encode() is probably expensive
                    children_merkle_value_concat.extend_from_slice(&merkle_value.encode());
                } else {
                    missing_children.push((
                        combined_key.clone(),
                        Some(child_index.clone()),
                        child_partial_key.clone(),
                    ));
                }
            }

            // We can't process further. Push back the current node on the queue, plus each of its
            // children.
            if !missing_children.is_empty() {
                queue_to_process.push((parent_key, child_index, partial_key));
                queue_to_process.extend(missing_children);
                continue;
            }

            (children_bitmap, children_merkle_value_concat)
        };

        // Starting from here, we are guaranteed to have all the information needed to finish the
        // computation of the merkle value.

        // Turn the `partial_key` into bytes with a weird encoding.
        let partial_key_hex_encode = {
            let partial_key = &partial_key.nibbles;
            if partial_key.len() % 2 == 0 {
                let mut pk = Vec::with_capacity(partial_key.len() / 2);
                for chunk in partial_key.chunks(2) {
                    pk.push((chunk[0].0 << 4) | chunk[1].0);
                }
                pk
            } else {
                let mut pk = Vec::with_capacity(1 + partial_key.len() / 2);
                pk.push(partial_key[0].0);
                for chunk in partial_key[1..].chunks(2) {
                    pk.push((chunk[0].0 << 4) | chunk[1].0);
                }
                pk
            }
        };

        // Load the stored value of this node.
        // TODO: do this in a more elegant way
        let stored_value = if combined_key.nibbles.len() % 2 == 0 {
            trie_access
                .clone()
                .get(&combined_key.to_bytes_truncate())
                .map(|v| v.as_ref().to_vec())
        } else {
            None
        };

        // Compute the header of the node.
        let header = {
            // The first two most significant bits of the header contain the type of node.
            let two_msb: u8 = {
                let has_stored_value = stored_value.is_some();
                let has_children = children_bitmap != 0;
                match (has_stored_value, has_children) {
                    (false, false) => {
                        // This should only ever be reached if we compute the root node of an
                        // empty trie.
                        debug_assert!(combined_key.nibbles.is_empty());
                        0b00
                    }
                    (true, false) => 0b01,
                    (false, true) => 0b10,
                    (true, true) => 0b11,
                }
            };

            // Another weird algorithm to encode the partial key length into the header.
            let mut pk_len = partial_key.nibbles.len();
            if pk_len >= 63 {
                pk_len -= 63;
                let mut header = vec![(two_msb << 6) + 63];
                while pk_len > 255 {
                    pk_len -= 255;
                    header.push(255);
                }
                header.push(u8::try_from(pk_len).unwrap());
                header
            } else {
                vec![(two_msb << 6) + u8::try_from(pk_len).unwrap()]
            }
        };

        // Compute the node subvalue.
        let node_subvalue = {
            if children_bitmap == 0 {
                if let Some(stored_value) = stored_value {
                    // TODO: SCALE-encoding clones the value; optimize that
                    stored_value.encode()
                } else {
                    Vec::new()
                }
            } else {
                let mut out = children_bitmap.to_le_bytes().to_vec();
                out.extend(children_merkle_value_concat);
                if let Some(stored_value) = stored_value {
                    // TODO: SCALE-encoding clones the value; optimize that
                    out.extend(stored_value.encode())
                }
                out
            }
        };

        // The node value is the concatenation of all these elements.
        let mut node_value = header;
        node_value.extend(partial_key_hex_encode);
        node_value.extend(node_subvalue);

        // The merkle value is either directly the node value, or a hash of the node value.
        let merkle_value = {
            if child_index.is_none() || node_value.len() >= 32 {
                let blake2_hash = blake2_rfc::blake2b::blake2b(32, &[], &node_value);
                debug_assert_eq!(blake2_hash.as_bytes().len(), 32);
                blake2_hash.as_bytes().to_vec()
            } else {
                debug_assert!(node_value.len() < 32);
                node_value
            }
        };

        // Insert the result in the cache.
        cache.entries.get_mut(&combined_key).unwrap().merkle_value = Some(merkle_value);
    }

    // Some sanity check.
    debug_assert!(cache
        .entries
        .contains_key(cache.root_node.as_ref().unwrap()));
    debug_assert!(cache
        .entries
        .get(cache.root_node.as_ref().unwrap())
        .unwrap()
        .merkle_value
        .is_some());
}

/// Returns all the keys of the nodes that descend from `key`, excluding `key` itself.
///
/// Always returns the children in order.
// TODO: implement in a cleaner way
fn child_nodes<'a>(
    trie_access: impl TrieRef<'a>,
    key: &TrieNodeKey,
) -> impl ExactSizeIterator<Item = (Nibble, TrieNodeKey)> {
    let mut key_clone = key.clone();
    key_clone.nibbles.push(Nibble(0));

    let mut out = Vec::new();
    for n in 0..16 {
        *key_clone.nibbles.last_mut().unwrap() = Nibble(n);
        let descendants =
            descendant_storage_keys(trie_access.clone(), &key_clone).collect::<Vec<_>>();
        debug_assert!(descendants
            .iter()
            .all(|k| TrieNodeKey::from_bytes(k.as_ref())
                .nibbles
                .starts_with(&key_clone.nibbles)));
        if let Some(prefix) = common_prefix(descendants.iter().map(|k| k.as_ref())) {
            debug_assert_ne!(prefix, *key);
            debug_assert!(prefix.nibbles.starts_with(&key.nibbles));
            let nibble = prefix.nibbles[key.nibbles.len()].clone();
            let partial_key = TrieNodeKey {
                nibbles: prefix.nibbles[(key.nibbles.len() + 1)..].to_vec(),
            };
            out.push((nibble, partial_key));
        }
    }
    out.into_iter()
}

/// Returns all the keys that descend from `key` or equal to `key` that have a storage entry.
// TODO: ugh, these lifetimes
fn descendant_storage_keys<'a>(
    trie_access: impl TrieRef<'a>,
    key: &TrieNodeKey,
) -> impl Iterator<Item = Vec<u8>> {
    // Because `prefix_keys` accepts only `&[u8]`, we pass a truncated version of the key
    // and filter out the returned elements that are not actually descendants.
    let list = {
        let equiv_full_bytes = key.to_bytes_truncate();
        trie_access
            .prefix_keys(&equiv_full_bytes)
            .filter(move |k| key.is_ancestor_or_equal(k.as_ref()))
            .map(|k| k.as_ref().to_vec())
            .collect::<Vec<Vec<u8>>>()
    };

    list.into_iter()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct TrieNodeKey {
    nibbles: Vec<Nibble>,
}

impl TrieNodeKey {
    fn empty() -> Self {
        TrieNodeKey {
            nibbles: Vec::new(),
        }
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        let mut out = Vec::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push(Nibble(*b >> 4));
            out.push(Nibble(*b & 0xf));
        }
        TrieNodeKey { nibbles: out }
    }

    fn to_bytes_truncate(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.nibbles.len() / 2);
        for n in self.nibbles.chunks(2) {
            debug_assert!(!n.is_empty());
            if n.len() < 2 {
                debug_assert_eq!(n.len(), 1);
                continue;
            }
            let byte = (n[0].0 << 4) | n[1].0;
            out.push(byte);
        }
        out
    }

    fn is_ancestor_or_equal(&self, key: &[u8]) -> bool {
        // TODO: make this code clearer
        let this = self.to_bytes_truncate();
        if self.nibbles.len() % 2 == 0 {
            // Truncation is actually not truncating.
            key.starts_with(&this)
        } else {
            // A nibble has been removed.
            let last_nibble = self.nibbles.last().unwrap().0;
            key.starts_with(&this) && key != &this[..] && (key[this.len()] >> 4) == last_nibble
        }
    }
}

/// Four bits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Nibble(u8);

/// Given a list of `&[u8]`, returns the longest prefix that is shared by all the elements in the
/// list.
fn common_prefix<'a>(mut list: impl Iterator<Item = &'a [u8]>) -> Option<TrieNodeKey> {
    let mut longest_prefix = TrieNodeKey::from_bytes(list.next()?);

    while let Some(elem) = list.next() {
        let elem = TrieNodeKey::from_bytes(elem);

        if elem.nibbles.len() < longest_prefix.nibbles.len() {
            longest_prefix.nibbles.truncate(elem.nibbles.len());
        }

        if let Some((diff_pos, _)) = longest_prefix
            .nibbles
            .iter()
            .enumerate()
            .find(|(idx, b)| elem.nibbles[*idx] != **b)
        {
            longest_prefix.nibbles.truncate(diff_pos);
        }

        if longest_prefix.nibbles.is_empty() {
            // No need to iterate further if the common prefix is already empty.
            break;
        }
    }

    Some(longest_prefix)
}

// TODO: tests

// TODO: add a test that generates a random trie, calculates its root using a cache, modifies it
// randomly, invalidating the cache in the process, then calculates the root again, once with
// cache and once without cache, and compares the two values
