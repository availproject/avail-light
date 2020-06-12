//! Freestanding function that calculates the root of a radix-16 Merkle-Patricia trie.
//!
//! See the parent module documentation for an explanation of what the trie is.

// TODO: add usage example

use alloc::{borrow::Cow, collections::BTreeMap};
use core::convert::TryFrom as _;
use hashbrown::{hash_map::Entry, HashMap};
use parity_scale_codec::Encode as _;

/// How to access the trie.
// TODO: make async
pub struct Config<'a, 'b> {
    /// Function that returns the value associated to a key. Returns `None` if there is no
    /// storage value.
    ///
    /// Must always return the same value if called multiple times with the same key.
    pub get_value: &'a dyn Fn(&[u8]) -> Option<&'b [u8]>,

    /// Function that returns the list of keys with values that start with the given prefix.
    ///
    /// All the keys returned must start with the given prefix. It is an error to omit a key
    /// from the result.
    pub prefix_keys: &'a dyn Fn(&[u8]) -> Vec<Cow<'b, [u8]>>,
    // TODO: add an optional calculation cache parameter
}

/// Calculates the Merkle value of the root node.
pub fn root_merkle_value(config: &Config) -> [u8; 32] {
    // The root node is not necessarily the one with an empty key. Just like any other node,
    // the root might have been merged with its lone children.

    // TODO: probably very slow, as we enumerate every single key in the storage
    let keys = (config.prefix_keys)(&[]);
    let key_from_root = common_prefix(keys.iter().map(|k| &**k)).unwrap_or(TrieNodeKey {
        nibbles: Vec::new(),
    });

    let val_vec = merkle_value(
        config,
        TrieNodeKey {
            nibbles: Vec::new(),
        },
        None,
        key_from_root,
    );

    let mut out = [0; 32];
    out.copy_from_slice(&val_vec);
    out
}

/// Calculates the Merkle value of the node whose key is the concatenation of `parent_key`,
/// `child_index`, and `partial_key`.
fn merkle_value(
    config: &Config,
    parent_key: TrieNodeKey,
    child_index: Option<Nibble>,
    partial_key: TrieNodeKey,
) -> Vec<u8> {
    let is_root = child_index.is_none();

    let node_value = node_value(config, parent_key, child_index, partial_key);

    if is_root || node_value.len() >= 32 {
        let blake2_hash = blake2_rfc::blake2b::blake2b(32, &[], &node_value);
        debug_assert_eq!(blake2_hash.as_bytes().len(), 32);
        blake2_hash.as_bytes().to_vec()
    } else {
        debug_assert!(node_value.len() < 32);
        node_value
    }
}

/// Calculates the node value of the node whose key is the concatenation of `parent_key`,
/// `child_index`, and `partial_key`.
fn node_value(
    config: &Config,
    parent_key: TrieNodeKey,
    child_index: Option<Nibble>,
    partial_key: TrieNodeKey,
) -> Vec<u8> {
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

    // The operations below require the actual key of the node.
    let combined_key = {
        let mut combined_key = parent_key;
        if let Some(child_index) = &child_index {
            combined_key.nibbles.push(child_index.clone());
        }
        combined_key.nibbles.extend(partial_key.nibbles.clone());
        combined_key
    };

    // Load the stored value of this node.
    let stored_value = if combined_key.nibbles.len() % 2 == 0 {
        (config.get_value)(&combined_key.to_bytes_truncate()).map(|v| v.to_vec())
    } else {
        None
    };

    // This "children bitmap" is filled below with bits if a child is present at the given
    // index.
    let mut children_bitmap = 0u16;
    // Keys from this node to its children.
    let mut children_partial_keys = Vec::<(Nibble, TrieNodeKey)>::new();

    // Now enumerate the children.
    for child in child_nodes(config, &combined_key) {
        debug_assert_ne!(child, combined_key);
        debug_assert!(child.nibbles.starts_with(&combined_key.nibbles));
        let child_index = child.nibbles[combined_key.nibbles.len()].clone();
        children_bitmap |= 1 << u32::from(child_index.0);

        let child_partial_key = TrieNodeKey {
            nibbles: child.nibbles[combined_key.nibbles.len() + 1..].to_vec(),
        };
        children_partial_keys.push((child_index, child_partial_key));
    }

    // Now compute the header of the node.
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
            for (child_index, child_partial_key) in children_partial_keys {
                let child_merkle_value = merkle_value(
                    config,
                    combined_key.clone(),
                    Some(child_index),
                    child_partial_key,
                );
                // TODO: we encode the child merkle value as SCALE, which copies it again; opt  imize that
                out.extend(child_merkle_value.encode());
            }
            if let Some(stored_value) = stored_value {
                // TODO: SCALE-encoding clones the value; optimize that
                out.extend(stored_value.encode())
            }
            out
        }
    };

    // Compute the final node value.
    let mut node_value = header;
    node_value.extend(partial_key_hex_encode);
    node_value.extend(node_subvalue);
    node_value
}

/// Returns all the keys of the nodes that descend from `key`, excluding `key` itself.
fn child_nodes(config: &Config, key: &TrieNodeKey) -> impl Iterator<Item = TrieNodeKey> {
    let mut key_clone = key.clone();
    key_clone.nibbles.push(Nibble(0));

    let mut out = Vec::new();
    for n in 0..16 {
        *key_clone.nibbles.last_mut().unwrap() = Nibble(n);
        let descendants = descendant_storage_keys(config, &key_clone).collect::<Vec<_>>();
        debug_assert!(
            descendants.iter().all(|k| TrieNodeKey::from_bytes(k)
                .nibbles
                .starts_with(&key_clone.nibbles)),
            "{:?} vs {:?}",
            descendants,
            key_clone
        ); // TODO: remove that extra debug once we're confident
        if let Some(prefix) = common_prefix(descendants.iter().map(|k| &**k)) {
            debug_assert_ne!(prefix, *key);
            out.push(prefix);
        }
    }
    out.into_iter()
}

/// Returns all the keys that descend from `key` or equal to `key` that have a storage entry.
fn descendant_storage_keys<'a>(
    config: &'a Config,
    key: &'a TrieNodeKey,
) -> impl Iterator<Item = Cow<'a, [u8]>> + 'a {
    // Because `config.prefix_keys` accepts only `&[u8]`, we pass a truncated version of the key
    // and filter out the returned elements that are not actually descendants.
    let equiv_full_bytes = key.to_bytes_truncate();
    (config.prefix_keys)(&equiv_full_bytes)
        .into_iter()
        .filter(move |k| key.is_ancestor_or_equal(&k))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct TrieNodeKey {
    nibbles: Vec<Nibble>,
}

impl TrieNodeKey {
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
