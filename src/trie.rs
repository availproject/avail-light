//! Radix-16 Merkle-Patricia trie.
//!
//! This Substrate/Polkadot-specific radix-16 Merkle-Patricia trie is a data structure that
//! associates keys with values, and that allows efficient verification of the integrity of the
//! data.
//!
//! This data structure is a tree composed of nodes, each node being identified by a key. A key
//! consists in a sequence of 4-bits values called *nibbles*. Example key: `[3, 12, 7, 0]`.
//!
//! Some of these nodes contain a value. These values are inserted by calling [`Trie::insert`].
//!
//! A node A is an *ancestor* of another node B if the key of A is a prefix of the key of B. For
//! example, the node whose key is `[3, 12]` is an ancestor of the node whose key is
//! `[3, 12, 8, 9]`. B is a *descendant* of A.
//!
//! Nodes exist only either if they contain a value, or if their key is the longest shared prefix
//! of two or more nodes that contain a value. For example, if nodes `[7, 2, 9, 11]` and
//! `[7, 2, 14, 8]` contain a value, then node `[7, 2]` also exist, because it is the longest
//! prefix shared between the two.
//!
//! The *Merkle value* of a node is composed, amongst other things, of its associated value and of
//! the Merkle value of its descendants. As such, modifying a node modifies the Merkle value of
//! all its ancestors. Note, however, that modifying a node modifies the Merkle value of *only*
//! its ancestors. As such, the time spent calculating the Merkle value of the root node of a trie
//! mostly depends on the number of modifications that are performed on it, and only a bit on the
//! size of the trie.

use alloc::collections::BTreeMap;
use core::convert::TryFrom as _;
use hashbrown::{hash_map::Entry, HashMap};
use parity_scale_codec::Encode as _;

/// Radix-16 Merkle-Patricia trie.
pub struct Trie {
    /// The entries in the tree.
    ///
    /// Since this is a binary tree, the elements are ordered lexicographically.
    /// Example order: "a", "ab", "ac", "b".
    ///
    /// This list only contains the nodes that have an entry in the storage, and not the nodes
    /// that are branches and don't have a storage entry.
    ///
    /// All the keys have an even number of nibbles.
    entries: BTreeMap<TrieNodeKey, Vec<u8>>,
}

impl Trie {
    /// Builds a new empty [`Trie`].
    pub fn new() -> Trie {
        Trie {
            entries: BTreeMap::new(),
        }
    }

    /// Inserts a new entry in the trie.
    pub fn insert(&mut self, key: &[u8], value: impl Into<Vec<u8>>) {
        self.entries
            .insert(TrieNodeKey::from_bytes(key), value.into());
    }

    /// Removes an entry from the trie.
    pub fn remove(&mut self, key: &[u8]) -> Option<Vec<u8>> {
        self.entries.remove(&TrieNodeKey::from_bytes(key))
    }

    /// Returns true if the `Trie` is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Removes all the elements from the trie.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Removes from the trie all the keys that start with `prefix`, including `prefix` itself.
    pub fn remove_prefix(&mut self, prefix: &[u8]) {
        if prefix.is_empty() {
            self.clear();
            return;
        }

        let prefix = TrieNodeKey::from_bytes(prefix);
        let to_remove = self
            .descendant_storage_keys(&prefix)
            .cloned()
            .collect::<Vec<_>>();
        for to_remove in to_remove {
            self.entries.remove(&to_remove);
        }
    }

    /// Calculates the Merkle value of the root node.
    pub fn root_merkle_value(&self) -> [u8; 32] {
        // The root node is not necessarily the one with an empty key. Just like any other node,
        // the root might have been merged with its lone children.

        let key_from_root = common_prefix(
            self.descendant_storage_keys(&TrieNodeKey {
                nibbles: Vec::new(),
            })
            .map(|k| &k.nibbles[..]),
        )
        .unwrap_or(Vec::new());

        let mut out = [0; 32];
        let val_vec = self.merkle_value(
            TrieNodeKey {
                nibbles: Vec::new(),
            },
            None,
            TrieNodeKey {
                nibbles: key_from_root,
            },
        );
        out.copy_from_slice(&val_vec);
        out
    }

    /// Calculates the Merkle value of the node whose key is the concatenation of `parent_key`,
    /// `child_index`, and `partial_key`.
    fn merkle_value(
        &self,
        parent_key: TrieNodeKey,
        child_index: Option<Nibble>,
        partial_key: TrieNodeKey,
    ) -> Vec<u8> {
        let is_root = child_index.is_none();

        let node_value = self.node_value(parent_key, child_index, partial_key);

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
        &self,
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
        let stored_value = self.entries.get(&combined_key).cloned();

        // This "children bitmap" is filled below with bits if a child is present at the given
        // index.
        let mut children_bitmap = 0u16;
        // Keys from this node to its children.
        let mut children_partial_keys = Vec::<(Nibble, TrieNodeKey)>::new();

        // Now enumerate the children.
        for child in self.child_nodes(&combined_key) {
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
                    let child_merkle_value = self.merkle_value(
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
    fn child_nodes(&self, key: &TrieNodeKey) -> impl Iterator<Item = TrieNodeKey> {
        let mut key_clone = key.clone();
        key_clone.nibbles.push(Nibble(0));

        let mut out = Vec::new();
        for n in 0..16 {
            *key_clone.nibbles.last_mut().unwrap() = Nibble(n);
            if let Some(prefix) = common_prefix(
                self.descendant_storage_keys(&key_clone)
                    .map(|k| &k.nibbles[..]),
            ) {
                out.push(TrieNodeKey { nibbles: prefix });
            }
        }
        out.into_iter()
    }

    /// Returns all the keys that descend from `key` or equal to `key` that have a storage entry.
    fn descendant_storage_keys<'a>(
        &'a self,
        key: &'a TrieNodeKey,
    ) -> impl Iterator<Item = &'a TrieNodeKey> + 'a {
        self.entries
            .range(key..)
            .take_while(move |(k, _)| key.is_ancestor_or_equal(&k.nibbles))
            .map(|(k, _v)| k)
    }
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

    fn is_ancestor_or_equal(&self, key: &[Nibble]) -> bool {
        let mut my_nibbles = self.nibbles.iter();
        let mut key_nibbles = key.iter();
        loop {
            match (my_nibbles.next(), key_nibbles.next()) {
                (Some(a), Some(b)) if a == b => {}
                (None, _) => return true,
                _ => return false,
            }
        }
    }
}

/// Four bits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Nibble(u8);

/// Given a list of `&[Nibble]`, returns the longest prefix that is shared by all the elements in
/// the list.
fn common_prefix<'a>(mut list: impl Iterator<Item = &'a [Nibble]>) -> Option<Vec<Nibble>> {
    let mut longest_prefix = list.next()?.to_vec();

    while let Some(elem) = list.next() {
        if elem.len() < longest_prefix.len() {
            longest_prefix.truncate(elem.len());
        }

        if let Some((diff_pos, _)) = longest_prefix
            .iter()
            .enumerate()
            .find(|(idx, b)| elem[*idx] != **b)
        {
            longest_prefix.truncate(diff_pos);
        }

        if longest_prefix.is_empty() {
            // No need to iterate further if the common prefix is already empty.
            break;
        }
    }

    Some(longest_prefix)
}

// TODO: remove testing private methods once we have better tests
#[cfg(test)]
mod tests {
    use super::{common_prefix, Nibble, Trie, TrieNodeKey};
    use core::iter;

    #[test]
    fn common_prefix_works_trivial() {
        let a = vec![Nibble(0)];

        let obtained = common_prefix([&a[..]].iter().cloned());
        assert_eq!(obtained, Some(a));
    }

    #[test]
    fn common_prefix_works_empty() {
        let obtained = common_prefix(iter::empty());
        assert_eq!(obtained, None);
    }

    #[test]
    fn common_prefix_works_basic() {
        let a = vec![Nibble(5), Nibble(4), Nibble(6)];
        let b = vec![Nibble(5), Nibble(4), Nibble(9), Nibble(12)];

        let obtained = common_prefix([&a[..], &b[..]].iter().cloned());
        assert_eq!(obtained, Some(vec![Nibble(5), Nibble(4)]));
    }

    #[test]
    fn trie_root_one_node() {
        let mut trie = Trie::new();
        trie.insert(b"abcd", b"hello world".to_vec());
        let hash = trie.root_merkle_value();
        // TODO: compare against expected
    }

    #[test]
    fn trie_root_unhashed_empty() {
        let trie = Trie::new();
        let obtained = trie.node_value(
            TrieNodeKey {
                nibbles: Vec::new(),
            },
            None,
            TrieNodeKey {
                nibbles: Vec::new(),
            },
        );
        assert_eq!(obtained, vec![0x0]);
    }

    #[test]
    fn trie_root_unhashed_single_tuple() {
        let mut trie = Trie::new();
        trie.insert(&[0xaa], [0xbb].to_vec());
        let obtained = trie.node_value(
            TrieNodeKey {
                nibbles: Vec::new(),
            },
            None,
            TrieNodeKey::from_bytes(&[0xaa]),
        );

        fn to_compact(n: u8) -> u8 {
            use parity_scale_codec::Encode as _;
            parity_scale_codec::Compact(n).encode()[0]
        }

        assert_eq!(
            obtained,
            vec![
                0x42,          // leaf 0x40 (2^6) with (+) key of 2 nibbles (0x02)
                0xaa,          // key data
                to_compact(1), // length of value in bytes as Compact
                0xbb           // value data
            ]
        );
    }

    #[test]
    fn trie_root_unhashed() {
        let mut trie = Trie::new();
        trie.insert(&[0x48, 0x19], [0xfe].to_vec());
        trie.insert(&[0x13, 0x14], [0xff].to_vec());

        let obtained = trie.node_value(
            TrieNodeKey {
                nibbles: Vec::new(),
            },
            None,
            TrieNodeKey {
                nibbles: Vec::new(),
            },
        );

        fn to_compact(n: u8) -> u8 {
            use parity_scale_codec::Encode as _;
            parity_scale_codec::Compact(n).encode()[0]
        }

        let mut ex = Vec::<u8>::new();
        ex.push(0x80); // branch, no value (0b_10..) no nibble
        ex.push(0x12); // slots 1 & 4 are taken from 0-7
        ex.push(0x00); // no slots from 8-15
        ex.push(to_compact(0x05)); // first slot: LEAF, 5 bytes long.
        ex.push(0x43); // leaf 0x40 with 3 nibbles
        ex.push(0x03); // first nibble
        ex.push(0x14); // second & third nibble
        ex.push(to_compact(0x01)); // 1 byte data
        ex.push(0xff); // value data
        ex.push(to_compact(0x05)); // second slot: LEAF, 5 bytes long.
        ex.push(0x43); // leaf with 3 nibbles
        ex.push(0x08); // first nibble
        ex.push(0x19); // second & third nibble
        ex.push(to_compact(0x01)); // 1 byte data
        ex.push(0xfe); // value data

        assert_eq!(obtained, ex);
    }
}
