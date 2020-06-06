//! Radix-16 Merkle-Patricia trie.
// TODO: write docs

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
        self.merkle_value(
            TrieNodeKey {
                nibbles: Vec::new(),
            },
            TrieNodeKey {
                nibbles: Vec::new(),
            },
        )
    }

    /// Calculates the Merkle value of the node whose key is the concatenation of `parent_key`
    /// and `partial_key`.
    fn merkle_value(&self, parent_key: TrieNodeKey, partial_key: TrieNodeKey) -> [u8; 32] {
        let is_root = parent_key.nibbles.is_empty() && partial_key.nibbles.is_empty();

        let node_value = self.node_value(parent_key, partial_key);

        if is_root || node_value.len() >= 32 {
            let blake2_hash = blake2_rfc::blake2b::blake2b(32, &[], &node_value);
            let mut out = [0; 32];
            out.copy_from_slice(blake2_hash.as_bytes());
            out
        } else {
            debug_assert!(node_value.len() < 32);
            let mut out = [0; 32];
            // TODO: specs mention that the return value is always 32bits, but are unclear how to
            // extend a less than 32bits value to 32bits
            out[(32 - node_value.len())..].copy_from_slice(&node_value);
            out
        }
    }

    /// Calculates the node value of the node whose key is the concatenation of `parent_key`
    /// and `partial_key`.
    fn node_value(&self, parent_key: TrieNodeKey, partial_key: TrieNodeKey) -> Vec<u8> {
        // Turn the `partial_key` into bytes with a weird encoding.
        let partial_key_bytes = if partial_key.nibbles.len() % 2 == 0 {
            let mut pk = Vec::with_capacity(partial_key.nibbles.len() / 2);
            for chunk in partial_key.nibbles.chunks(2) {
                pk.push((chunk[0].0 << 4) | chunk[1].0);
            }
            pk
        } else {
            let mut pk = Vec::with_capacity(1 + partial_key.nibbles.len() / 2);
            pk.push(partial_key.nibbles[0].0);
            for chunk in partial_key.nibbles[1..].chunks(2) {
                pk.push((chunk[0].0 << 4) | chunk[1].0);
            }
            pk
        };

        // The operations below require the actual key of the node.
        let combined_key = {
            let mut combined_key = parent_key;
            combined_key.nibbles.extend(partial_key.nibbles.clone());
            combined_key
        };

        // Load the stored value of this node.
        let stored_value = self.entries.get(&combined_key).cloned();

        // This "children bitmap" is filled below with bits if a child is present at the given
        // index.
        let mut children_bitmap = 0u16;
        // Partial key from this node to its children.
        let mut children_partial_keys = Vec::new();

        // Now enumerate the children.
        for child in self.child_nodes(&combined_key) {
            debug_assert!(child.nibbles.starts_with(&combined_key.nibbles));
            let child_index = child.nibbles[combined_key.nibbles.len()].0;
            children_bitmap |= 1 << u32::from(child_index);

            let child_partial_key = TrieNodeKey {
                nibbles: child.nibbles[combined_key.nibbles.len()..].to_vec(),
            };
            children_partial_keys.push(child_partial_key);
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
            // TODO: SCALE-encoding clones the value; optimize that
            let encoded_stored_value = stored_value.unwrap_or(Vec::new()).encode();

            if children_bitmap == 0 {
                encoded_stored_value
            } else {
                // TODO: specs don't say anything about endianess or bits ordering of
                // children_bitmap; had to look in Substrate code; report that to specs writers
                let mut out = children_bitmap.to_le_bytes().to_vec();
                for child in children_partial_keys {
                    let child_merkle_value = self.merkle_value(combined_key.clone(), child);
                    // TODO: we encode the child merkle value as SCALE, which copies it again; opt  imize that
                    out.extend(child_merkle_value.encode());
                }
                out.extend(encoded_stored_value);
                out
            }
        };

        // Compute the final node value.
        let mut node_value = header;
        node_value.extend(partial_key_bytes);
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

// TODO: we test private methods below because the code doesn't work at the moment
#[cfg(test)]
mod tests {
    use super::{common_prefix, Nibble};
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
}
