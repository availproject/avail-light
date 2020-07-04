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

use super::{nibble::Nibble, node_value, trie_structure};

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
        /*let structure = match &mut self.structure {
            Some(s) => s,
            None => return,
        };

        fn bytes_to_nibbles(bytes: &[u8]) -> Vec<Nibble> {
            bytes
                .iter()
                .map(|b| Nibble::try_from(*b).unwrap())
                .collect()
        }

        match (structure.node(&bytes_to_nibbles(key)), has_value) {
            (trie_structure::Entry::Vacant(entry), true) => {
                match entry.insert_storage_value()
            }
            (trie_structure::Entry::Vacant(_), false) => {}
            (trie_structure::Entry::Occupied(entry), true) => {

            }
            (trie_structure::Entry::Occupied(trie_structure::NodeAccess::Branch(_)), false) => {}
            (trie_structure::Entry::Occupied(trie_structure::NodeAccess::Storage(entry)), false) => {
                match entry.remove() {
                    trie_structure::Remove::StorageToBranch(_) => {}
                    trie_structure::Remove::BranchAlsoRemoved { sibling, .. } => {
                        sibling.user_data().merkle_value = None;
                    }
                    trie_structure::Remove::SingleRemove { child: Some(child), .. } => {
                        child
                    }
                    trie_structure::Remove::SingleRemove { child: None, .. } => {}
                }
            }
        }*/
        self.structure = None; // TODO:
    }

    /// Notify the cache that all the storage values whose key start with the given prefix have
    /// been removed.
    pub fn prefix_remove_update(&mut self, prefix: &[u8]) {
        self.structure = None; // TODO:
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
            children: &(0..16).map(|_| None).collect::<Vec<_>>(),
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
                let key = key
                    .as_ref()
                    .iter()
                    .flat_map(|byte| {
                        let n1 = Nibble::try_from(byte >> 4).unwrap();
                        let n2 = Nibble::try_from(byte & 0xf).unwrap();
                        iter::once(n1).chain(iter::once(n2))
                    })
                    .collect::<Vec<_>>();
                // TODO: ideally we'd pass an iterator to `node()` so that we don't allocate a Vec here
                structure
                    .node(&key)
                    .into_vacant()
                    .unwrap()
                    .insert_storage_value()
                    .insert(Default::default(), Default::default());
            }
            structure
        });
    }

    // `trie_structure` is guaranteed to match the trie, but its Merkle values might be missing.
    let trie_structure = cache.structure.as_mut().unwrap();
    trie_structure.shrink_to_fit();

    // Each iteration in the code below pops the last element of this queue, fills it in the cache,
    // and optionally pushes new elements to the queue. Once the queue is empty, the function
    // returns.
    //
    // Each element in the queue contains the node's key.
    let mut queue_to_process: Vec<Vec<Nibble>> = Vec::new();
    if let Some(mut root_node) = trie_structure.root_node() {
        if root_node.user_data().merkle_value.is_none() {
            queue_to_process.push(root_node.full_key().collect());
        }
    }

    loop {
        let node_key = match queue_to_process.pop() {
            Some(p) => p,
            None => return,
        };

        // Build the Merkle value of all the children of the node. Necessary for the computation
        // of the Merkle value of the node itself.
        //
        // We do this first, because we might have to interrupt the processing of this node if
        // one of the children doesn't have a cache entry.
        let children = {
            // TODO: shouldn't we first check whether all the children are in cache to speed
            // things up and avoid extra copies? should benchmark this

            // TODO: review this code below for unnecessary allocations

            // TODO: ArrayVec
            let mut children = vec![None; 16];

            // Will be set to the children whose merkle value of a child is missing from the cache.
            // TODO: ArrayVec
            let mut missing_children = Vec::with_capacity(16);

            for child_index in 0..16u8 {
                // TODO: a bit annoying that we traverse the trie all the time
                let node = trie_structure
                    .existing_node(node_key.iter().cloned())
                    .unwrap();
                let mut child = match node.child(Nibble::try_from(child_index).unwrap()) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                if let Some(merkle_value) = &child.user_data().merkle_value {
                    children[usize::from(child_index)] = Some(merkle_value.clone());
                } else {
                    missing_children.push(child.full_key().collect());
                }
            }

            // We can't process further. Push back the current node on the queue, plus each of its
            // children.
            if !missing_children.is_empty() {
                queue_to_process.push(node_key);
                queue_to_process.extend(missing_children);
                continue;
            }

            children
        };

        // Load the stored value of this node.
        // TODO: do this in a more elegant way
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
        let mut node = trie_structure
            .existing_node(node_key.iter().cloned())
            .unwrap();
        let merkle_value = node_value::calculate_merke_root(node_value::Config {
            is_root: node.is_root_node(),
            children: &children,
            partial_key: node.partial_key(),
            stored_value,
        });
        node.user_data().merkle_value = Some(merkle_value);
    }
}

// TODO: tests

// TODO: add a test that generates a random trie, calculates its root using a cache, modifies it
// randomly, invalidating the cache in the process, then calculates the root again, once with
// cache and once without cache, and compares the two values
