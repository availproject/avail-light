// Smoldot
// Copyright (C) 2019-2021  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Manages the structure of a trie. Allows inserting and removing nodes, but does not store any
//! value. Only the structure is stored.
//!
//! See the [`TrieStructure`] struct.

use super::nibble::Nibble;

use alloc::{borrow::ToOwned as _, vec::Vec};
use core::{convert::TryFrom as _, fmt, iter};
use either::Either;
use slab::Slab;

mod tests;

/// Stores the structure of a trie, including branch nodes that have no storage value.
///
/// The `TUd` parameter is a user data stored in each node.
///
/// This struct doesn't represent a complete trie. It only manages the structure of the trie, and
/// the storage values have to be maintained in parallel of this.
pub struct TrieStructure<TUd> {
    /// List of nodes. Using a [`Slab`] guarantees that the node indices never change.
    nodes: Slab<Node<TUd>>,
    /// Index of the root node within [`TrieStructure::nodes`]. `None` if the trie is empty.
    root_index: Option<usize>,
}

/// Entry in the structure.
#[derive(Debug)]
struct Node<TUd> {
    /// Index of the parent within [`TrieStructure::nodes`]. `None` if this is the root.
    parent: Option<(usize, Nibble)>,
    /// Partial key of the node. Portion to add to the values in `parent` to obtain the full key.
    partial_key: Vec<Nibble>,
    /// Indices of the children within [`TrieStructure::nodes`].
    children: [Option<usize>; 16],
    /// If true, this node is a so-called "storage node" with a storage value associated to it. If
    /// false, then it is a so-called "branch node". Branch nodes are automatically removed from
    /// the trie if their number of children is inferior to 2.
    has_storage_value: bool,
    /// User data associated to the node.
    user_data: TUd,
}

impl<TUd> TrieStructure<TUd> {
    /// Builds a new empty trie.
    ///
    /// Equivalent to calling [`TrieStructure::with_capacity`] with a capacity of 0.
    ///
    /// # Examples
    ///
    /// ```
    /// use smoldot::trie::trie_structure;
    ///
    /// let trie = trie_structure::TrieStructure::<()>::new();
    /// assert!(trie.is_empty());
    /// assert_eq!(trie.capacity(), 0);
    /// ```
    pub fn new() -> Self {
        TrieStructure {
            nodes: Slab::new(),
            root_index: None,
        }
    }

    /// Builds a new empty trie with a capacity for the given number of nodes.
    ///
    /// # Examples
    ///
    /// ```
    /// use smoldot::trie::trie_structure;
    ///
    /// let trie = trie_structure::TrieStructure::<()>::with_capacity(12);
    /// assert!(trie.is_empty());
    /// assert_eq!(trie.capacity(), 12);
    /// ```
    pub fn with_capacity(capacity: usize) -> Self {
        TrieStructure {
            nodes: Slab::with_capacity(capacity),
            root_index: None,
        }
    }

    /// Returns the number of nodes (storage or branch nodes) the trie can hold without
    /// reallocating.
    ///
    /// # Examples
    ///
    /// ```
    /// use smoldot::trie::trie_structure;
    ///
    /// let trie = trie_structure::TrieStructure::<()>::with_capacity(7);
    /// assert_eq!(trie.capacity(), 7);
    /// ```
    pub fn capacity(&self) -> usize {
        self.nodes.capacity()
    }

    /// Returns `true` if the trie doesn't contain any node.
    ///
    /// Equivalent to [`TrieStructure::len`] returning 0.
    ///
    /// # Examples
    ///
    /// ```
    /// use smoldot::trie::{self, trie_structure};
    ///
    /// let mut trie = trie_structure::TrieStructure::new();
    /// assert!(trie.is_empty());
    ///
    /// // Insert a node.
    /// trie
    ///     .node(trie::bytes_to_nibbles(b"foo".iter().cloned()))
    ///     .into_vacant()
    ///     .unwrap()
    ///     .insert_storage_value()
    ///     .insert((), ());
    /// assert!(!trie.is_empty());
    ///
    /// // Remove the newly-inserted node.
    /// trie
    ///     .node(trie::bytes_to_nibbles(b"foo".iter().cloned()))
    ///     .into_occupied()
    ///     .unwrap()
    ///     .into_storage()
    ///     .unwrap()
    ///     .remove();
    /// assert!(trie.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns the number of nodes, both branch and storage nodes, in the trie structure.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Reduces the capacity of the trie as much as possible.
    ///
    /// See [`Vec::shrink_to_fit`].
    pub fn shrink_to_fit(&mut self) {
        self.nodes.shrink_to_fit();
    }

    /// Returns the root node of the trie, or `None` if the trie is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use smoldot::trie::{self, trie_structure};
    ///
    /// let mut trie = trie_structure::TrieStructure::new();
    /// assert!(trie.root_node().is_none());
    ///
    /// // Insert a node. It becomes the root.
    /// trie
    ///     .node(trie::bytes_to_nibbles(b"foo".iter().cloned()))
    ///     .into_vacant()
    ///     .unwrap()
    ///     .insert_storage_value()
    ///     .insert((), ());
    ///
    /// assert!(trie.root_node().is_some());
    /// assert!(trie.root_node().unwrap().parent().is_none());
    /// ```
    pub fn root_node(&mut self) -> Option<NodeAccess<TUd>> {
        Some(self.node_by_index_inner(self.root_index?).unwrap())
    }

    /// Returns an [`Entry`] corresponding to the node whose key is the concatenation of the list
    /// of nibbles passed as parameter.
    ///
    /// # Examples
    ///
    /// ```
    /// use smoldot::trie::{self, trie_structure};
    ///
    /// let mut trie = trie_structure::TrieStructure::new();
    ///
    /// let node: trie_structure::Entry<_, _> = trie
    ///     .node(trie::bytes_to_nibbles(b"ful".iter().cloned()));
    ///
    /// match node {
    ///     // `Occupied` is returned if a node with this key exists.
    ///     trie_structure::Entry::Occupied(_) => unreachable!(),
    ///
    ///     // `Vacant` is returned if no node with this key exists yet.
    ///     // In this example, a node is inserted.
    ///     trie_structure::Entry::Vacant(entry) => {
    ///         entry.insert_storage_value().insert((), ())
    ///     },
    /// };
    ///
    /// // The same node can for example be queried again.
    /// // This time, it will be in the `Occupied` state.
    /// match trie.node(trie::bytes_to_nibbles(b"ful".iter().cloned())) {
    ///     // `NodeAccess::Storage` is used if the node has been explicitly inserted.
    ///     trie_structure::Entry::Occupied(trie_structure::NodeAccess::Storage(_)) => {},
    ///
    ///     // `Branch` would be returned if this was a branch node. See below.
    ///     trie_structure::Entry::Occupied(trie_structure::NodeAccess::Branch(_)) => {
    ///         unreachable!()
    ///     },
    ///     trie_structure::Entry::Vacant(e) => unreachable!(),
    /// };
    ///
    /// // In order to demonstrate branch nodes, let's insert a node at the key `fez`.
    /// trie
    ///     .node(trie::bytes_to_nibbles(b"fez".iter().cloned()))
    ///     .into_vacant()
    ///     .unwrap()
    ///     .insert_storage_value()
    ///     .insert((), ());
    ///
    /// // The trie now contains not two but three nodes. A branch node whose key is `f` has
    /// // automatically been inserted as the parent of both `ful` and `fez`.
    /// assert_eq!(trie.len(), 3);
    /// match trie.node(trie::bytes_to_nibbles(b"f".iter().cloned())) {
    ///     trie_structure::Entry::Occupied(trie_structure::NodeAccess::Branch(_)) => {},
    ///     _ => unreachable!(),
    /// };
    /// ```
    pub fn node<TKIter>(&mut self, key: TKIter) -> Entry<TUd, TKIter>
    where
        TKIter: Iterator<Item = Nibble> + Clone,
    {
        match self.existing_node_inner(key.clone()) {
            ExistingNodeInnerResult::Found {
                node_index,
                has_storage_value: true,
            } => Entry::Occupied(NodeAccess::Storage(StorageNodeAccess {
                trie: self,
                node_index,
            })),
            ExistingNodeInnerResult::Found {
                node_index,
                has_storage_value: false,
            } => Entry::Occupied(NodeAccess::Branch(BranchNodeAccess {
                trie: self,
                node_index,
            })),
            ExistingNodeInnerResult::NotFound { closest_ancestor } => Entry::Vacant(Vacant {
                trie: self,
                key,
                closest_ancestor,
            }),
        }
    }

    /// Returns the node with the given key, or `None` if no such node exists.
    ///
    /// This method is a shortcut for calling [`TrieStructure::node`] followed with
    /// [`Entry::into_occupied`].
    pub fn existing_node(&mut self, key: impl Iterator<Item = Nibble>) -> Option<NodeAccess<TUd>> {
        if let ExistingNodeInnerResult::Found {
            node_index,
            has_storage_value,
        } = self.existing_node_inner(key)
        {
            Some(if has_storage_value {
                NodeAccess::Storage(StorageNodeAccess {
                    trie: self,
                    node_index,
                })
            } else {
                NodeAccess::Branch(BranchNodeAccess {
                    trie: self,
                    node_index,
                })
            })
        } else {
            None
        }
    }

    /// Inner implementation of [`TrieStructure::existing_node`]. Traverses the tree, trying to
    /// find a node whose key is `key`.
    fn existing_node_inner(
        &mut self,
        mut key: impl Iterator<Item = Nibble>,
    ) -> ExistingNodeInnerResult {
        let mut current_index = match self.root_index {
            Some(ri) => ri,
            None => {
                return ExistingNodeInnerResult::NotFound {
                    closest_ancestor: None,
                }
            }
        };
        debug_assert!(self.nodes.get(current_index).unwrap().parent.is_none());

        let mut closest_ancestor = None;

        loop {
            let current = self.nodes.get(current_index).unwrap();

            // First, we must remove `current`'s partial key from `key`, making sure that they
            // match.
            for nibble in current.partial_key.iter().cloned() {
                if key.next() != Some(nibble) {
                    return ExistingNodeInnerResult::NotFound { closest_ancestor };
                }
            }

            // At this point, the tree traversal cursor (the `key` iterator) exactly matches
            // `current`.
            // If `key.next()` is `Some`, put it in `child_index`, otherwise return successfully.
            let child_index = match key.next() {
                Some(n) => n,
                None => {
                    return ExistingNodeInnerResult::Found {
                        node_index: current_index,
                        has_storage_value: current.has_storage_value,
                    }
                }
            };

            closest_ancestor = Some(current_index);

            if let Some(next_index) = current.children[usize::from(u8::from(child_index))] {
                current_index = next_index;
            } else {
                return ExistingNodeInnerResult::NotFound { closest_ancestor };
            }
        }
    }

    /// Removes all nodes whose key starts with the given prefix.
    ///
    /// Returns the closest ancestors to the nodes that have been removed, or `None` if that
    /// closest ancestor is not a descendant of the new trie root.
    pub fn remove_prefix(
        &mut self,
        _prefix: impl Iterator<Item = Nibble> + Clone,
    ) -> Option<NodeAccess<TUd>> {
        todo!() // TODO: implement and write example
                /*// `ancestor` is the node that will stay in the tree and the common ancestor of all the
                // nodes to remove.
                let ancestor = match self.existing_node_inner(prefix.clone()) {
                    ExistingNodeInnerResult::Found { node_index, .. } => {
                        self.nodes.get(node_index).unwrap().parent
                    }
                    ExistingNodeInnerResult::NotFound {
                        closest_ancestor: None,
                    } => None,
                    ExistingNodeInnerResult::NotFound {
                        closest_ancestor: Some(ancestor),
                    } => {
                        let key_len = self.node_full_key(ancestor).count();
                        let child_index = prefix.skip(key_len).next().unwrap();
                        Some((ancestor, child_index))
                    }
                };

                // If `ancestor` is `None`, .

                if let Some((ancestor_index, child_index)) = ancestor {
                    let to_drop = self.nodes.get_mut(ancestor_index).unwrap().children
                        [usize::from(u8::from(child_index))]
                    .take();
                    if let Some(to_drop) = to_drop {
                        // TODO: ! we leak if we don't do anything here
                    }
                }*/
    }

    /// Returns true if the structure of this trie is the same as the structure of `other`.
    ///
    /// Everything is compared for equality except for the user datas.
    ///
    /// > **Note**: This function does a preliminary check for `self.len() == other.len()`. If the
    /// >           length are different, `false` is immediately returned. If the lengths are
    /// >           equal, the function performs the expensive operation of traversing both
    /// >           tries in order to detect a potential mismatch.
    ///
    /// # Examples
    ///
    /// ```
    /// use smoldot::trie::{self, trie_structure};
    ///
    /// let mut trie1 = trie_structure::TrieStructure::new();
    /// let mut trie2 = trie_structure::TrieStructure::new();
    /// assert!(trie1.structure_equal(&trie2));
    ///
    /// // Insert a node in the first trie.
    /// trie1
    ///     .node(trie::bytes_to_nibbles(b"foo".iter().cloned()))
    ///     .into_vacant()
    ///     .unwrap()
    ///     .insert_storage_value()
    ///     .insert(1234, 5678);
    /// assert!(!trie1.structure_equal(&trie2));
    ///
    /// // Insert the same node in the second trie, but with a different user data.
    /// // The type of the user data of the second trie (strings) isn't even the same as for the
    /// // first trie (i32s).
    /// trie2
    ///     .node(trie::bytes_to_nibbles(b"foo".iter().cloned()))
    ///     .into_vacant()
    ///     .unwrap()
    ///     .insert_storage_value()
    ///     .insert("hello", "world");
    ///
    /// // `structure_equal` returns true because both tries have the same nodes.
    /// assert!(trie1.structure_equal(&trie2));
    /// ```
    pub fn structure_equal<T>(&self, other: &TrieStructure<T>) -> bool {
        if self.nodes.len() != other.nodes.len() {
            return false;
        }

        let mut me_iter = self.all_nodes_ordered();
        let mut other_iter = other.all_nodes_ordered();

        loop {
            let (me_node_idx, other_node_idx) = match (me_iter.next(), other_iter.next()) {
                (Some(a), Some(b)) => (a, b),
                (None, None) => return true,
                _ => return false,
            };

            let me_node = self.nodes.get(me_node_idx).unwrap();
            let other_node = other.nodes.get(other_node_idx).unwrap();

            if me_node.has_storage_value != other_node.has_storage_value {
                return false;
            }

            match (me_node.parent, other_node.parent) {
                (Some((_, i)), Some((_, j))) if i == j => {}
                (None, None) => {}
                _ => return false,
            }

            if me_node.partial_key != other_node.partial_key {
                return false;
            }
        }
    }

    /// Iterates over all nodes of the trie, in a specific order.
    fn all_nodes_ordered<'b>(&'b self) -> impl Iterator<Item = usize> + 'b {
        if let Some(root_index) = self.root_index {
            either::Either::Left(iter::once(root_index).chain(self.descendants(root_index)))
        } else {
            either::Either::Right(iter::empty())
        }
    }

    /// Returns the [`NodeAccess`] of the node at the given index, or `None` if no such node
    /// exists.
    ///
    /// # Context
    ///
    /// Each node inserted in the trie is placed in the underlying data structure at a specific
    /// [`NodeIndex`] that never changes until the node is removed from the trie.
    ///
    /// This [`NodeIndex`] can be retreived by calling [`NodeAccess::node_index`],
    /// [`StorageNodeAccess::node_index`] or [`BranchNodeAccess::node_index`]. The same node can
    /// later be accessed again by calling [`TrieStructure::node_by_index`].
    ///
    /// A [`NodeIndex`] value can be reused after its previous node has been removed.
    ///
    /// # Examples
    ///
    /// ```
    /// use smoldot::trie::{self, trie_structure};
    ///
    /// let mut trie = trie_structure::TrieStructure::new();
    ///
    /// // Insert an example node.
    /// let inserted_node = trie
    ///     .node(trie::bytes_to_nibbles(b"foo".iter().cloned()))
    ///     .into_vacant()
    ///     .unwrap()
    ///     .insert_storage_value()
    ///     .insert(12, 80);
    /// let node_index = inserted_node.node_index();
    /// drop(inserted_node);  // Drops the borrow to this node.
    ///
    /// // At this point, no borrow of the `trie` object exists anymore.
    ///
    /// // Later, the same node can be accessed again.
    /// let mut node = trie.node_by_index(node_index).unwrap();
    /// assert_eq!(*node.user_data(), 12);
    /// ```
    pub fn node_by_index(&mut self, node_index: NodeIndex) -> Option<NodeAccess<TUd>> {
        self.node_by_index_inner(node_index.0)
    }

    /// Internal function. Returns the [`NodeAccess`] of the node at the given index.
    fn node_by_index_inner(&mut self, node_index: usize) -> Option<NodeAccess<TUd>> {
        if self.nodes.get(node_index)?.has_storage_value {
            Some(NodeAccess::Storage(StorageNodeAccess {
                trie: self,
                node_index,
            }))
        } else {
            Some(NodeAccess::Branch(BranchNodeAccess {
                trie: self,
                node_index,
            }))
        }
    }

    /// Returns the key of the node at the given index, or `None` if no such node exists.
    ///
    /// This method is a shortcut for [`TrieStructure::node_by_index`] followed with
    /// [`NodeAccess::full_key`].
    pub fn node_full_key_by_index<'b>(
        &'b self,
        node_index: NodeIndex,
    ) -> Option<impl Iterator<Item = Nibble> + 'b> {
        if !self.nodes.contains(node_index.0) {
            return None;
        }

        Some(self.node_full_key(node_index.0))
    }

    /// Returns the full key of the node with the given index.
    ///
    /// # Panic
    ///
    /// Panics if `target` is not a valid index.
    fn node_full_key<'b>(&'b self, target: usize) -> impl Iterator<Item = Nibble> + 'b {
        self.node_path(target)
            .chain(iter::once(target))
            .flat_map(move |n| {
                let node = self.nodes.get(n).unwrap();
                let child_index = node.parent.into_iter().map(|p| p.1);
                let partial_key = node.partial_key.iter().cloned();
                child_index.chain(partial_key)
            })
    }

    /// Returns the indices of the nodes to traverse to reach `target`. The returned iterator
    /// does *not* include `target`. In other words, if `target` is the root node, this returns
    /// an empty iterator.
    ///
    /// # Panic
    ///
    /// Panics if `target` is not a valid index.
    fn node_path<'b>(&'b self, target: usize) -> impl Iterator<Item = usize> + 'b {
        debug_assert!(self.nodes.get(usize::max_value()).is_none());
        // First element is an invalid key, each successor is the last element of
        // `reverse_node_path(target)` that isn't equal to `current`.
        // Since the first element is invalid, we skip it.
        // Since `reverse_node_path` never produces `target`, we know that it also won't be
        // produced here.
        iter::successors(Some(usize::max_value()), move |&current| {
            self.reverse_node_path(target)
                .take_while(move |n| *n != current)
                .last()
        })
        .skip(1)
    }

    /// Returns the indices of the nodes starting from `target` towards the root node. The returned
    /// iterator does *not* include `target` but does include the root node if it is different
    /// from `target`. If `target` is the root node, this returns an empty iterator.
    ///
    /// # Panic
    ///
    /// Panics if `target` is not a valid index.
    fn reverse_node_path<'b>(&'b self, target: usize) -> impl Iterator<Item = usize> + 'b {
        // First element is `target`, each successor is `current.parent`.
        // Since `target` must explicitly not be included, we skip the first element.
        iter::successors(Some(target), move |current| {
            Some(self.nodes.get(*current).unwrap().parent?.0)
        })
        .skip(1)
    }

    /// Returns the indices of all the descendants (direct or indirect) of `node_index`, not
    /// including `node_index` itself.
    ///
    /// # Panic
    ///
    /// Panics if `node_index` is not a valid index.
    fn descendants<'b>(&'b self, node_index: usize) -> impl Iterator<Item = usize> + 'b {
        // First element is `node_index`. Each successor is the first child of `current` or,
        // if `current` doesn't have any children, the next sibling of `current`.
        // Since `node_index` must explicitly not be included, we skip the first element.
        iter::successors(Some(node_index), move |current| {
            let first_child = self
                .nodes
                .get(*current)
                .unwrap()
                .children
                .iter()
                .filter_map(|c| *c)
                .next();
            if let Some(first_child) = first_child {
                Some(first_child)
            } else {
                self.next_sibling(*current)
            }
        })
        .skip(1)
    }

    /// Returns the next sibling of the given node.
    ///
    /// # Panic
    ///
    /// Panics if `node_index` is not a valid index.
    fn next_sibling(&self, node_index: usize) -> Option<usize> {
        let (parent_index, child_index) = self.nodes.get(node_index).unwrap().parent?;
        let parent = self.nodes.get(parent_index).unwrap();

        for idx in (u8::from(child_index) + 1)..16 {
            if let Some(child) = parent.children[usize::from(idx)] {
                return Some(child);
            }
        }

        None
    }
}

impl<TUd> Default for TrieStructure<TUd> {
    fn default() -> Self {
        Self::new()
    }
}

impl<TUd: fmt::Debug> fmt::Debug for TrieStructure<TUd> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list()
            .entries(
                self.all_nodes_ordered()
                    .map(|idx| (idx, self.nodes.get(idx).unwrap())),
            )
            .finish()
    }
}

enum ExistingNodeInnerResult {
    Found {
        node_index: usize,
        has_storage_value: bool,
    },
    NotFound {
        /// Closest ancestor that actually exists.
        closest_ancestor: Option<usize>,
    },
}

/// Index of a node in the trie. Never invalidated, except when if node in question is destroyed.
///
/// See [`TrieStructure::node_by_index`] for more information.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct NodeIndex(usize);

/// Access to a entry for a potential node within the [`TrieStructure`].
///
/// See [`TrieStructure::node`] for more information.
pub enum Entry<'a, TUd, TKIter> {
    /// There exists a node with this key.
    Occupied(NodeAccess<'a, TUd>),
    /// This entry is vacant.
    Vacant(Vacant<'a, TUd, TKIter>),
}

impl<'a, TUd, TKIter> Entry<'a, TUd, TKIter> {
    /// Returns `Some` if `self` is an [`Entry::Vacant`].
    pub fn into_vacant(self) -> Option<Vacant<'a, TUd, TKIter>> {
        match self {
            Entry::Vacant(e) => Some(e),
            _ => None,
        }
    }

    /// Returns `Some` if `self` is an [`Entry::Occupied`].
    pub fn into_occupied(self) -> Option<NodeAccess<'a, TUd>> {
        match self {
            Entry::Occupied(e) => Some(e),
            _ => None,
        }
    }
}

/// Access to a node within the [`TrieStructure`].
pub enum NodeAccess<'a, TUd> {
    Storage(StorageNodeAccess<'a, TUd>),
    Branch(BranchNodeAccess<'a, TUd>),
}

impl<'a, TUd> NodeAccess<'a, TUd> {
    /// Returns `Some` if `self` is an [`NodeAccess::Storage`].
    pub fn into_storage(self) -> Option<StorageNodeAccess<'a, TUd>> {
        match self {
            NodeAccess::Storage(e) => Some(e),
            _ => None,
        }
    }

    /// Returns an opaque [`NodeIndex`] representing the node in the trie.
    ///
    /// It can later be used to retrieve this same node using [`TrieStructure::node_by_index`].
    pub fn node_index(&self) -> NodeIndex {
        match self {
            NodeAccess::Storage(n) => n.node_index(),
            NodeAccess::Branch(n) => n.node_index(),
        }
    }

    /// Returns the parent of this node, or `None` if this is the root node.
    pub fn into_parent(self) -> Option<NodeAccess<'a, TUd>> {
        match self {
            NodeAccess::Storage(n) => n.into_parent(),
            NodeAccess::Branch(n) => n.into_parent(),
        }
    }

    /// Returns the parent of this node, or `None` if this is the root node.
    pub fn parent(&mut self) -> Option<NodeAccess<TUd>> {
        match self {
            NodeAccess::Storage(n) => n.parent(),
            NodeAccess::Branch(n) => n.parent(),
        }
    }

    /// Returns the user data of the child at the given index.
    ///
    /// > **Note**: This method exists because it accepts `&self` rather than `&mut self`. A
    /// >           cleaner alternative would be to split the [`NodeAccess`] struct into
    /// >           `NodeAccessRef` and `NodeAccessMut`, but that's a lot of efforts compare to
    /// >           this single method.
    pub fn child_user_data(&self, index: Nibble) -> Option<&TUd> {
        match self {
            NodeAccess::Storage(n) => n.child_user_data(index),
            NodeAccess::Branch(n) => n.child_user_data(index),
        }
    }

    /// Returns the child of this node at the given index.
    pub fn child(&mut self, index: Nibble) -> Option<NodeAccess<TUd>> {
        match self {
            NodeAccess::Storage(n) => n.child(index),
            NodeAccess::Branch(n) => n.child(index),
        }
    }

    /// Returns the child of this node given the given index.
    ///
    /// Returns back `self` if there is no such child at this index.
    pub fn into_child(self, index: Nibble) -> Result<NodeAccess<'a, TUd>, Self> {
        match self {
            NodeAccess::Storage(n) => n.into_child(index).map_err(NodeAccess::Storage),
            NodeAccess::Branch(n) => n.into_child(index).map_err(NodeAccess::Branch),
        }
    }

    /// Returns the first child of this node.
    ///
    /// Returns back `self` if this node doesn't have any children.
    pub fn into_first_child(self) -> Result<NodeAccess<'a, TUd>, Self> {
        match self {
            NodeAccess::Storage(n) => n.into_first_child().map_err(NodeAccess::Storage),
            NodeAccess::Branch(n) => n.into_first_child().map_err(NodeAccess::Branch),
        }
    }

    /// Returns the next sibling of this node.
    ///
    /// Returns back `self` if this node is the last child of its parent.
    pub fn into_next_sibling(self) -> Result<NodeAccess<'a, TUd>, Self> {
        match self {
            NodeAccess::Storage(n) => n.into_next_sibling().map_err(NodeAccess::Storage),
            NodeAccess::Branch(n) => n.into_next_sibling().map_err(NodeAccess::Branch),
        }
    }

    /// Returns true if this node is the root node of the trie.
    pub fn is_root_node(&self) -> bool {
        match self {
            NodeAccess::Storage(n) => n.is_root_node(),
            NodeAccess::Branch(n) => n.is_root_node(),
        }
    }

    /// Returns the full key of the node.
    pub fn full_key<'b>(&'b self) -> impl Iterator<Item = Nibble> + 'b {
        match self {
            NodeAccess::Storage(n) => Either::Left(n.full_key()),
            NodeAccess::Branch(n) => Either::Right(n.full_key()),
        }
    }

    /// Returns the partial key of the node.
    pub fn partial_key<'b>(&'b self) -> impl ExactSizeIterator<Item = Nibble> + 'b {
        match self {
            NodeAccess::Storage(n) => Either::Left(n.partial_key()),
            NodeAccess::Branch(n) => Either::Right(n.partial_key()),
        }
    }

    /// Returns the user data stored in the node.
    pub fn user_data(&mut self) -> &mut TUd {
        match self {
            NodeAccess::Storage(n) => n.user_data(),
            NodeAccess::Branch(n) => n.user_data(),
        }
    }

    /// Returns true if the node has a storage value associated to it.
    pub fn has_storage_value(&self) -> bool {
        match self {
            NodeAccess::Storage(_) => true,
            NodeAccess::Branch(_) => false,
        }
    }
}

/// Access to a node within the [`TrieStructure`] that is known to have a storage value associated
/// to it.
pub struct StorageNodeAccess<'a, TUd> {
    trie: &'a mut TrieStructure<TUd>,
    node_index: usize,
}

impl<'a, TUd> StorageNodeAccess<'a, TUd> {
    /// Returns an opaque [`NodeIndex`] representing the node in the trie.
    ///
    /// It can later be used to retrieve this same node using [`TrieStructure::node_by_index`].
    pub fn node_index(&self) -> NodeIndex {
        NodeIndex(self.node_index)
    }

    /// Returns the parent of this node, or `None` if this is the root node.
    pub fn into_parent(self) -> Option<NodeAccess<'a, TUd>> {
        let parent_idx = self.trie.nodes.get(self.node_index).unwrap().parent?.0;
        Some(self.trie.node_by_index_inner(parent_idx).unwrap())
    }

    /// Returns the parent of this node, or `None` if this is the root node.
    pub fn parent(&mut self) -> Option<NodeAccess<TUd>> {
        let parent_idx = self.trie.nodes.get(self.node_index).unwrap().parent?.0;
        Some(self.trie.node_by_index_inner(parent_idx).unwrap())
    }

    /// Returns the first child of this node.
    ///
    /// Returns back `self` if this node doesn't have any children.
    pub fn into_first_child(self) -> Result<NodeAccess<'a, TUd>, Self> {
        let first_child_idx = self
            .trie
            .nodes
            .get(self.node_index)
            .unwrap()
            .children
            .iter()
            .filter_map(|c| *c)
            .next();

        let first_child_idx = match first_child_idx {
            Some(fc) => fc,
            None => return Err(self),
        };

        Ok(self.trie.node_by_index_inner(first_child_idx).unwrap())
    }

    /// Returns the next sibling of this node.
    ///
    /// Returns back `self` if this node is the last child of its parent.
    pub fn into_next_sibling(self) -> Result<NodeAccess<'a, TUd>, Self> {
        let next_sibling_idx = match self.trie.next_sibling(self.node_index) {
            Some(ns) => ns,
            None => return Err(self),
        };

        Ok(self.trie.node_by_index_inner(next_sibling_idx).unwrap())
    }

    /// Returns the child of this node at the given index.
    pub fn child(&mut self, index: Nibble) -> Option<NodeAccess<TUd>> {
        let child_idx =
            self.trie.nodes.get(self.node_index).unwrap().children[usize::from(u8::from(index))]?;
        Some(self.trie.node_by_index_inner(child_idx).unwrap())
    }

    /// Returns the user data of the child at the given index.
    ///
    /// > **Note**: This method exists because it accepts `&self` rather than `&mut self`. A
    /// >           cleaner alternative would be to split the [`NodeAccess`] struct into
    /// >           `NodeAccessRef` and `NodeAccessMut`, but that's a lot of efforts compare to
    /// >           this single method.
    pub fn child_user_data(&self, index: Nibble) -> Option<&TUd> {
        let child_idx =
            self.trie.nodes.get(self.node_index).unwrap().children[usize::from(u8::from(index))]?;
        Some(&self.trie.nodes.get(child_idx).unwrap().user_data)
    }

    /// Returns the child of this node given the given index.
    ///
    /// Returns back `self` if there is no such child at this index.
    pub fn into_child(self, index: Nibble) -> Result<NodeAccess<'a, TUd>, Self> {
        let child_idx = match self.trie.nodes.get(self.node_index).unwrap().children
            [usize::from(u8::from(index))]
        {
            Some(c) => c,
            None => return Err(self),
        };

        Ok(self.trie.node_by_index_inner(child_idx).unwrap())
    }

    /// Returns true if this node is the root node of the trie.
    pub fn is_root_node(&self) -> bool {
        self.trie.root_index == Some(self.node_index)
    }

    /// Returns the full key of the node.
    pub fn full_key<'b>(&'b self) -> impl Iterator<Item = Nibble> + 'b {
        self.trie.node_full_key(self.node_index)
    }

    /// Returns the partial key of the node.
    pub fn partial_key<'b>(&'b self) -> impl ExactSizeIterator<Item = Nibble> + 'b {
        self.trie
            .nodes
            .get(self.node_index)
            .unwrap()
            .partial_key
            .iter()
            .cloned()
    }

    /// Returns the user data associated to this node.
    pub fn user_data(&mut self) -> &mut TUd {
        &mut self.trie.nodes.get_mut(self.node_index).unwrap().user_data
    }

    /// Removes the storage value and returns what this changes in the trie structure.
    pub fn remove(self) -> Remove<'a, TUd> {
        // If the removed node has 2 or more children, then the node continues as a branch node.
        {
            let mut node = self.trie.nodes.get_mut(self.node_index).unwrap();
            if node.children.iter().filter(|c| c.is_some()).count() >= 2 {
                node.has_storage_value = false;
                return Remove::StorageToBranch(BranchNodeAccess {
                    trie: self.trie,
                    node_index: self.node_index,
                });
            }
        }

        let removed_node = self.trie.nodes.remove(self.node_index);
        debug_assert!(removed_node.has_storage_value);

        // We already know from above that the removed node has only 0 or 1 children. Let's
        // determine which.
        let child_node_index: Option<usize> =
            removed_node.children.iter().filter_map(|c| *c).next();

        // If relevant, update our single child's parent to point to `removed_node`'s parent.
        if let Some(child_node_index) = child_node_index {
            let child = self.trie.nodes.get_mut(child_node_index).unwrap();
            debug_assert_eq!(child.parent.as_ref().unwrap().0, self.node_index);
            insert_front(
                &mut child.partial_key,
                removed_node.partial_key,
                child.parent.unwrap().1,
            );
            child.parent = removed_node.parent;
        }

        // At this point, we're almost done with removing `removed_node` from `self.trie`. However
        // there is potentially another change to make: maybe `parent` has to be removed from the
        // trie as well.

        // Update `parent`'s child to point to `child_node_index`.
        // `single_remove` is true if we can keep `parent` in the trie.
        let single_remove = if let Some((parent_index, parent_to_removed_child_index)) =
            removed_node.parent
        {
            // Update `removed_node`'s parent to point to the child.
            let parent = self.trie.nodes.get_mut(parent_index).unwrap();
            debug_assert_eq!(
                parent.children[usize::from(u8::from(parent_to_removed_child_index))],
                Some(self.node_index)
            );
            parent.children[usize::from(u8::from(parent_to_removed_child_index))] =
                child_node_index;

            // If `parent` does *not* need to be removed, we can return early.
            parent.has_storage_value || parent.children.iter().filter(|c| c.is_some()).count() >= 2
        } else {
            debug_assert_eq!(self.trie.root_index, Some(self.node_index));
            self.trie.root_index = child_node_index;
            true
        };

        // If we keep the parent in the trie, return early with a `SingleRemove`.
        if single_remove {
            return if let Some(child_node_index) = child_node_index {
                Remove::SingleRemoveChild {
                    user_data: removed_node.user_data,
                    child: self.trie.node_by_index_inner(child_node_index).unwrap(),
                }
            } else if let Some((parent_index, _)) = removed_node.parent {
                Remove::SingleRemoveNoChild {
                    user_data: removed_node.user_data,
                    parent: self.trie.node_by_index_inner(parent_index).unwrap(),
                }
            } else {
                debug_assert!(self.trie.nodes.is_empty());
                debug_assert!(self.trie.root_index.is_none());
                Remove::TrieNowEmpty {
                    user_data: removed_node.user_data,
                }
            };
        }

        // If we reach here, then parent has to be removed from the trie as well.
        let parent_index = removed_node.parent.unwrap().0;
        debug_assert!(child_node_index.is_none());
        let removed_branch = self.trie.nodes.remove(parent_index);
        debug_assert!(!removed_branch.has_storage_value);

        // We already know from above that the removed branch has exactly 1 sibling. Let's
        // determine which.
        debug_assert_eq!(
            removed_branch
                .children
                .iter()
                .filter(|c| c.is_some())
                .count(),
            1
        );
        let sibling_node_index: usize = removed_branch
            .children
            .iter()
            .filter_map(|c| *c)
            .next()
            .unwrap();

        // Update the sibling to point to the parent's parent.
        {
            let sibling = self.trie.nodes.get_mut(sibling_node_index).unwrap();
            debug_assert_eq!(sibling.parent.as_ref().unwrap().0, parent_index);
            insert_front(
                &mut sibling.partial_key,
                removed_branch.partial_key,
                sibling.parent.unwrap().1,
            );
            sibling.parent = removed_branch.parent;
        }

        // Update the parent's parent to point to the sibling.
        if let Some((parent_parent_index, parent_to_sibling_index)) = removed_branch.parent {
            // Update the parent's parent to point to the sibling.
            let parent_parent = self.trie.nodes.get_mut(parent_parent_index).unwrap();
            debug_assert_eq!(
                parent_parent.children[usize::from(u8::from(parent_to_sibling_index))],
                Some(parent_index)
            );
            parent_parent.children[usize::from(u8::from(parent_to_sibling_index))] =
                Some(sibling_node_index);
        } else {
            debug_assert_eq!(self.trie.root_index, Some(parent_index));
            self.trie.root_index = Some(sibling_node_index);
        }

        // Success!
        Remove::BranchAlsoRemoved {
            sibling: self.trie.node_by_index_inner(sibling_node_index).unwrap(),
            storage_user_data: removed_node.user_data,
            branch_user_data: removed_branch.user_data,
        }
    }
}

/// Outcome of the removal of a storage value.
pub enum Remove<'a, TUd> {
    /// Removing the storage value didn't change the structure of the trie. Contains a
    /// [`BranchNodeAccess`] representing the same node as the [`StorageNodeAccess`] whose value
    /// got removed.
    StorageToBranch(BranchNodeAccess<'a, TUd>),

    /// Removing the storage value removed the node that contained the storage value. Apart from
    /// this removal, the structure of the trie didn't change.
    ///
    /// The node that got removed had one single child. This child's parent becomes the parent
    /// that the former node had.
    ///
    /// ```text
    ///
    ///
    ///                +-+                                         +-+
    ///           +--> +-+ <--+                         +--------> +-+ <--+
    ///           |           |                         |                 |
    ///           |           +                         |                 +
    ///           |     (0 or more other children)      |           (0 or more other children)
    ///           |                                     |
    ///          +-+                                    |
    ///     +--> +-+ removed node                       |
    ///     |                                           |
    ///     |                                           |
    ///     |                                           |
    ///     |                                           |
    ///    +-+                                         +-+
    ///    +-+                                         +-+  `child`
    ///     ^                                           ^
    ///     ++ (0 or more other children)               ++ (0 or more other children)
    ///
    /// ```
    ///
    SingleRemoveChild {
        /// Unique child that the removed node had. The parent and partial key of this child has
        /// been modified.
        child: NodeAccess<'a, TUd>,

        /// User data that was in the removed node.
        user_data: TUd,
    },

    /// Removing the storage value removed the node that contained the storage value. Apart from
    /// this removal, the structure of the trie didn't change.
    ///
    /// The node that got removed didn't have any children.
    ///
    /// ```text
    ///
    ///       Before                                       After
    ///
    ///                                                        `parent`
    ///                +-+                                 +-+
    ///           +--> +-+ <--+                            +-+ <--+
    ///           |           |                                   |
    ///           |           +                                   +
    ///           |       (0 or more other children)          (0 or more other children)
    ///           |
    ///          +-+
    ///          +-+ removed node
    ///
    /// ```
    ///
    SingleRemoveNoChild {
        /// Parent that the removed node had.
        parent: NodeAccess<'a, TUd>,

        /// User data that was in the removed node.
        user_data: TUd,
    },

    /// The trie was empty apart from this node. It is now completely empty.
    TrieNowEmpty {
        /// User data that was in the removed node.
        user_data: TUd,
    },

    /// Removing the storage value removed two nodes from the trie: the one that contained the
    /// storage value and its parent, which was a branch node.
    ///
    /// This can only happen if the removed node had no children and only one sibling.
    ///
    /// ```text
    ///
    ///             Before                        After
    ///
    ///
    ///               +                             +
    ///               |                             |
    ///              +-+                            |
    ///         +--> +-+ <--+                       +-----+
    ///         |           |                             |
    ///         |           |                             |
    ///        +-+         +-+                           +-+
    ///        +-+         +-+                           +-+ `sibling`
    ///                     ^                             ^
    ///  removed node       |                             |
    ///                     +                             +
    ///                (0 or more other nodes)       (0 or more other nodes)
    ///
    /// ```
    ///
    BranchAlsoRemoved {
        /// Sibling of the removed node. The parent and partial key of this sibling have been
        /// modified.
        sibling: NodeAccess<'a, TUd>,

        /// User data that was in the removed storage node.
        storage_user_data: TUd,

        /// User data that was in the removed branch node (former parent of `storage_user_data`).
        branch_user_data: TUd,
    },
}

/// Access to a node within the [`TrieStructure`] that is known to not have any storage value
/// associated to it.
pub struct BranchNodeAccess<'a, TUd> {
    trie: &'a mut TrieStructure<TUd>,
    node_index: usize,
}

impl<'a, TUd> BranchNodeAccess<'a, TUd> {
    /// Returns an opaque [`NodeIndex`] representing the node in the trie.
    ///
    /// It can later be used to retrieve this same node using [`TrieStructure::node_by_index`].
    pub fn node_index(&self) -> NodeIndex {
        NodeIndex(self.node_index)
    }

    /// Returns the parent of this node, or `None` if this is the root node.
    pub fn into_parent(self) -> Option<NodeAccess<'a, TUd>> {
        let parent_idx = self.trie.nodes.get(self.node_index).unwrap().parent?.0;
        Some(self.trie.node_by_index_inner(parent_idx).unwrap())
    }

    /// Returns the parent of this node, or `None` if this is the root node.
    pub fn parent(&mut self) -> Option<NodeAccess<TUd>> {
        let parent_idx = self.trie.nodes.get(self.node_index).unwrap().parent?.0;
        Some(self.trie.node_by_index_inner(parent_idx).unwrap())
    }

    /// Returns the first child of this node.
    ///
    /// Returns back `self` if this node doesn't have any children.
    pub fn into_first_child(self) -> Result<NodeAccess<'a, TUd>, Self> {
        let first_child_idx = self
            .trie
            .nodes
            .get(self.node_index)
            .unwrap()
            .children
            .iter()
            .filter_map(|c| *c)
            .next();

        let first_child_idx = match first_child_idx {
            Some(fc) => fc,
            None => return Err(self),
        };

        Ok(self.trie.node_by_index_inner(first_child_idx).unwrap())
    }

    /// Returns the next sibling of this node.
    ///
    /// Returns back `self` if this node is the last child of its parent.
    pub fn into_next_sibling(self) -> Result<NodeAccess<'a, TUd>, Self> {
        let next_sibling_idx = match self.trie.next_sibling(self.node_index) {
            Some(ns) => ns,
            None => return Err(self),
        };

        Ok(self.trie.node_by_index_inner(next_sibling_idx).unwrap())
    }

    /// Returns the child of this node at the given index.
    pub fn child(&mut self, index: Nibble) -> Option<NodeAccess<TUd>> {
        let child_idx =
            self.trie.nodes.get(self.node_index).unwrap().children[usize::from(u8::from(index))]?;
        Some(self.trie.node_by_index_inner(child_idx).unwrap())
    }

    /// Returns the user data of the child at the given index.
    ///
    /// > **Note**: This method exists because it accepts `&self` rather than `&mut self`. A
    /// >           cleaner alternative would be to split the [`NodeAccess`] struct into
    /// >           `NodeAccessRef` and `NodeAccessMut`, but that's a lot of efforts compare to
    /// >           this single method.
    pub fn child_user_data(&self, index: Nibble) -> Option<&TUd> {
        let child_idx =
            self.trie.nodes.get(self.node_index).unwrap().children[usize::from(u8::from(index))]?;
        Some(&self.trie.nodes.get(child_idx).unwrap().user_data)
    }

    /// Returns the child of this node given the given index.
    ///
    /// Returns back `self` if there is no such child at this index.
    pub fn into_child(self, index: Nibble) -> Result<NodeAccess<'a, TUd>, Self> {
        let child_idx = match self.trie.nodes.get(self.node_index).unwrap().children
            [usize::from(u8::from(index))]
        {
            Some(c) => c,
            None => return Err(self),
        };

        Ok(self.trie.node_by_index_inner(child_idx).unwrap())
    }

    /// Returns true if this node is the root node of the trie.
    pub fn is_root_node(&self) -> bool {
        self.trie.root_index == Some(self.node_index)
    }

    /// Returns the full key of the node.
    pub fn full_key<'b>(&'b self) -> impl Iterator<Item = Nibble> + 'b {
        self.trie.node_full_key(self.node_index)
    }

    /// Returns the partial key of the node.
    pub fn partial_key<'b>(&'b self) -> impl ExactSizeIterator<Item = Nibble> + 'b {
        self.trie
            .nodes
            .get(self.node_index)
            .unwrap()
            .partial_key
            .iter()
            .cloned()
    }

    /// Adds a storage value to this node, turning it into a [`StorageNodeAccess`].
    ///
    /// The trie structure doesn't change.
    pub fn insert_storage_value(self) -> StorageNodeAccess<'a, TUd> {
        let mut node = self.trie.nodes.get_mut(self.node_index).unwrap();
        debug_assert!(!node.has_storage_value);
        node.has_storage_value = true;

        StorageNodeAccess {
            trie: self.trie,
            node_index: self.node_index,
        }
    }

    /// Returns the user data associated to this node.
    pub fn user_data(&mut self) -> &mut TUd {
        &mut self.trie.nodes.get_mut(self.node_index).unwrap().user_data
    }
}

/// Access to a non-existing node within the [`TrieStructure`].
pub struct Vacant<'a, TUd, TKIter> {
    trie: &'a mut TrieStructure<TUd>,
    /// Full key of the node to insert.
    key: TKIter,
    /// Known closest ancestor that is in `trie`. Will become the parent of any newly-inserted
    /// node.
    closest_ancestor: Option<usize>,
}

impl<'a, TUd, TKIter> Vacant<'a, TUd, TKIter>
where
    TKIter: Iterator<Item = Nibble> + Clone,
{
    /// Prepare the operation of creating the node in question.
    ///
    /// This method analyzes the trie to prepare for the operation, but doesn't actually perform
    /// any insertion. To perform the insertion, use the returned [`PrepareInsert`].
    pub fn insert_storage_value(mut self) -> PrepareInsert<'a, TUd> {
        // Retrieve what will be the parent after we insert the new node, not taking branching
        // into account yet.
        // If `Some`, contains its index and number of nibbles in its key.
        let future_parent = match (self.closest_ancestor, self.trie.root_index) {
            (Some(_), None) => unreachable!(),
            (Some(ancestor), Some(_)) => {
                let key_len = self.trie.node_full_key(ancestor).count();
                debug_assert!(self.key.clone().count() > key_len);
                Some((ancestor, key_len))
            }
            (None, Some(_)) => None,
            (None, None) => {
                // Situation where the trie is empty. This is kind of a special case that we
                // handle by returning early.
                return PrepareInsert::One(PrepareInsertOne {
                    trie: self.trie,
                    parent: None,
                    partial_key: self.key.collect(),
                    children: [None; 16],
                });
            }
        };

        // Get the existing child of `future_parent` that points towards the newly-inserted node,
        // or a successful early-return if none.
        let existing_node_index =
            if let Some((future_parent_index, future_parent_key_len)) = future_parent {
                let new_child_index = self.key.clone().nth(future_parent_key_len).unwrap();
                let future_parent = self.trie.nodes.get(future_parent_index).unwrap();
                match future_parent.children[usize::from(u8::from(new_child_index))] {
                    Some(i) => {
                        debug_assert_eq!(
                            self.trie.nodes.get(i).unwrap().parent.unwrap().0,
                            future_parent_index
                        );
                        i
                    }
                    None => {
                        // There is an empty slot in `future_parent` for our new node.
                        //
                        //
                        //           `future_parent`
                        //                 +-+
                        //             +-> +-+  <---------+
                        //             |        <----+    |
                        //             |     ^       |    |
                        //            +-+    |       |    |
                        //   New node +-+    +-+-+  +-+  +-+  0 or more existing children
                        //                     +-+  +-+  +-+
                        //
                        //
                        return PrepareInsert::One(PrepareInsertOne {
                            trie: self.trie,
                            parent: Some((future_parent_index, new_child_index)),
                            partial_key: self.key.skip(future_parent_key_len + 1).collect(),
                            children: [None; 16],
                        });
                    }
                }
            } else {
                self.trie.root_index.unwrap()
            };

        // `existing_node_idx` and the new node are known to either have the same parent and the
        // same child index, or to both have no parent. Now let's compare their partial key.
        let existing_node_partial_key = &self
            .trie
            .nodes
            .get(existing_node_index)
            .unwrap()
            .partial_key;
        let new_node_partial_key = self
            .key
            .clone()
            .skip(future_parent.map_or(0, |(_, n)| n + 1))
            .collect::<Vec<_>>();
        debug_assert_ne!(*existing_node_partial_key, new_node_partial_key);
        debug_assert!(!new_node_partial_key.starts_with(existing_node_partial_key));

        // If `existing_node_partial_key` starts with `new_node_partial_key`, then the new node
        // will be inserted in-between the parent and the existing node.
        if existing_node_partial_key.starts_with(&new_node_partial_key) {
            // The new node is to be inserted in-between `future_parent` and
            // `existing_node_index`.
            //
            // If `future_parent` is `Some`:
            //
            //
            //                         +-+
            //        `future_parent`  +-+ <---------+
            //                          ^            |
            //                          |            +
            //                         +-+         (0 or more existing children)
            //               New node  +-+
            //                          ^
            //                          |
            //                         +-+
            //  `existing_node_index`  +-+
            //                          ^
            //                          |
            //                          +
            //                    (0 or more existing children)
            //
            //
            //
            // If `future_parent` is `None`:
            //
            //
            //            New node    +-+
            //    (becomes the root)  +-+
            //                         ^
            //                         |
            // `existing_node_index`  +-+
            //     (current root)     +-+
            //                         ^
            //                         |
            //                         +
            //                   (0 or more existing children)
            //

            let mut new_node_children = [None; 16];
            let existing_node_new_child_index =
                existing_node_partial_key[new_node_partial_key.len()];
            new_node_children[usize::from(u8::from(existing_node_new_child_index))] =
                Some(existing_node_index);

            return PrepareInsert::One(PrepareInsertOne {
                trie: self.trie,
                parent: if let Some((future_parent_index, future_parent_key_len)) = future_parent {
                    let new_child_index = self.key.nth(future_parent_key_len).unwrap();
                    Some((future_parent_index, new_child_index))
                } else {
                    None
                },
                partial_key: new_node_partial_key,
                children: new_node_children,
            });
        }

        // If we reach here, we know that we will need to create a new branch node in addition to
        // the new storage node.
        //
        // If `future_parent` is `Some`:
        //
        //
        //                  `future_parent`
        //
        //                        +-+
        //                        +-+ <--------+  (0 or more existing children)
        //                         ^
        //                         |
        //       New branch node  +-+
        //                        +-+ <-------+
        //                         ^          |
        //                         |          |
        //                        +-+        +-+
        // `existing_node_index`  +-+        +-+  New storage node
        //                         ^
        //                         |
        //
        //                 (0 or more existing children)
        //
        //
        //
        // If `future_parent` is `None`:
        //
        //
        //     New branch node    +-+
        //     (becomes root)     +-+ <-------+
        //                         ^          |
        //                         |          |
        // `existing_node_index`  +-+        +-+
        //     (current root)     +-+        +-+  New storage node
        //                         ^
        //                         |
        //
        //                 (0 or more existing children)
        //
        //

        // Find the common ancestor between `new_node_partial_key` and `existing_node_partial_key`.
        let branch_partial_key_len = {
            debug_assert_ne!(new_node_partial_key, &**existing_node_partial_key);
            let mut len = 0;
            let mut k1 = new_node_partial_key.iter();
            let mut k2 = existing_node_partial_key.iter();
            // Since `new_node_partial_key` is different from `existing_node_partial_key`, we know
            // that `k1.next()` and `k2.next()` won't both be `None`.
            while k1.next() == k2.next() {
                len += 1;
            }
            debug_assert!(len < new_node_partial_key.len());
            debug_assert!(len < existing_node_partial_key.len());
            len
        };

        // Table of children for the new branch node, not including the new storage node.
        // It therefore contains only one entry: `existing_node_index`.
        let branch_children = {
            let mut branch_children = [None; 16];
            let existing_node_new_child_index = existing_node_partial_key[branch_partial_key_len];
            debug_assert_ne!(
                existing_node_new_child_index,
                new_node_partial_key[branch_partial_key_len]
            );
            branch_children[usize::from(u8::from(existing_node_new_child_index))] =
                Some(existing_node_index);
            branch_children
        };

        // Success!
        PrepareInsert::Two(PrepareInsertTwo {
            trie: self.trie,

            storage_child_index: new_node_partial_key[branch_partial_key_len],
            storage_partial_key: new_node_partial_key[branch_partial_key_len + 1..].to_owned(),

            branch_parent: if let Some((future_parent_index, future_parent_key_len)) = future_parent
            {
                let new_child_index = self.key.nth(future_parent_key_len).unwrap();
                Some((future_parent_index, new_child_index))
            } else {
                None
            },
            branch_partial_key: new_node_partial_key[..branch_partial_key_len].to_owned(),
            branch_children,
        })
    }
}

/// Preparation for a new node insertion.
///
/// The trie hasn't been modified yet and you can safely drop this object.
#[must_use]
pub enum PrepareInsert<'a, TUd> {
    /// One node will be inserted in the trie.
    One(PrepareInsertOne<'a, TUd>),
    /// Two nodes will be inserted in the trie.
    Two(PrepareInsertTwo<'a, TUd>),
}

impl<'a, TUd> PrepareInsert<'a, TUd> {
    /// Insert the new node. `branch_node_user_data` is discarded if `self` is
    /// a [`PrepareInsert::One`].
    pub fn insert(
        self,
        storage_node_user_data: TUd,
        branch_node_user_data: TUd,
    ) -> StorageNodeAccess<'a, TUd> {
        match self {
            PrepareInsert::One(n) => n.insert(storage_node_user_data),
            PrepareInsert::Two(n) => n.insert(storage_node_user_data, branch_node_user_data),
        }
    }
}

/// One node will be inserted in the trie.
pub struct PrepareInsertOne<'a, TUd> {
    trie: &'a mut TrieStructure<TUd>,

    /// Value of [`Node::parent`] for the newly-created node.
    /// If `None`, we also set the root of the trie to the new node.
    parent: Option<(usize, Nibble)>,
    /// Value of [`Node::partial_key`] for the newly-created node.
    partial_key: Vec<Nibble>,
    /// Value of [`Node::children`] for the newly-created node.
    children: [Option<usize>; 16],
}

impl<'a, TUd> PrepareInsertOne<'a, TUd> {
    /// Insert the new node.
    pub fn insert(self, user_data: TUd) -> StorageNodeAccess<'a, TUd> {
        let new_node_partial_key_len = self.partial_key.len();

        let new_node_index = self.trie.nodes.insert(Node {
            parent: self.parent,
            partial_key: self.partial_key,
            children: self.children,
            has_storage_value: true,
            user_data,
        });

        // Update the children node to point to their new parent.
        for (child_index, child) in self.children.iter().enumerate() {
            let mut child = match child {
                Some(c) => self.trie.nodes.get_mut(*c).unwrap(),
                None => continue,
            };

            let child_index = Nibble::try_from(u8::try_from(child_index).unwrap()).unwrap();
            child.parent = Some((new_node_index, child_index));
            truncate_first_elems(&mut child.partial_key, new_node_partial_key_len + 1);
        }

        // Update the parent to point to its new child.
        if let Some((parent_index, child_index)) = self.parent {
            let mut parent = self.trie.nodes.get_mut(parent_index).unwrap();
            parent.children[usize::from(u8::from(child_index))] = Some(new_node_index);
        } else {
            self.trie.root_index = Some(new_node_index);
        }

        // Success!
        StorageNodeAccess {
            trie: self.trie,
            node_index: new_node_index,
        }
    }
}

/// Two nodes will be inserted in the trie.
pub struct PrepareInsertTwo<'a, TUd> {
    trie: &'a mut TrieStructure<TUd>,

    /// Value of the child index in [`Node::parent`] for the newly-created storage node.
    storage_child_index: Nibble,
    /// Value of [`Node::partial_key`] for the newly-created storage node.
    storage_partial_key: Vec<Nibble>,

    /// Value of [`Node::parent`] for the newly-created branch node.
    /// If `None`, we also set the root of the trie to the new branch node.
    branch_parent: Option<(usize, Nibble)>,
    /// Value of [`Node::partial_key`] for the newly-created branch node.
    branch_partial_key: Vec<Nibble>,
    /// Value of [`Node::children`] for the newly-created branch node. Does not include the entry
    /// that must be filled with the new storage node.
    branch_children: [Option<usize>; 16],
}

impl<'a, TUd> PrepareInsertTwo<'a, TUd> {
    /// Key of the branch node that will be inserted.
    pub fn branch_node_key<'b>(&'b self) -> impl Iterator<Item = Nibble> + 'b {
        if let Some((parent_index, child_index)) = self.branch_parent {
            let parent = self.trie.node_full_key(parent_index);
            let iter = parent
                .chain(iter::once(child_index))
                .chain(self.branch_partial_key.iter().cloned());
            Either::Left(iter)
        } else {
            Either::Right(self.branch_partial_key.iter().cloned())
        }
    }

    /// Insert the new node.
    pub fn insert(
        self,
        storage_node_user_data: TUd,
        branch_node_user_data: TUd,
    ) -> StorageNodeAccess<'a, TUd> {
        let new_branch_node_partial_key_len = self.branch_partial_key.len();

        debug_assert_eq!(
            self.branch_children.iter().filter(|c| c.is_some()).count(),
            1
        );

        let new_branch_node_index = self.trie.nodes.insert(Node {
            parent: self.branch_parent,
            partial_key: self.branch_partial_key,
            children: self.branch_children,
            has_storage_value: false,
            user_data: branch_node_user_data,
        });

        let new_storage_node_index = self.trie.nodes.insert(Node {
            parent: Some((new_branch_node_index, self.storage_child_index)),
            partial_key: self.storage_partial_key,
            children: [None; 16],
            has_storage_value: true,
            user_data: storage_node_user_data,
        });

        self.trie
            .nodes
            .get_mut(new_branch_node_index)
            .unwrap()
            .children[usize::from(u8::from(self.storage_child_index))] =
            Some(new_storage_node_index);

        // Update the branch node's children to point to their new parent.
        for (child_index, child) in self.branch_children.iter().enumerate() {
            let mut child = match child {
                Some(c) => self.trie.nodes.get_mut(*c).unwrap(),
                None => continue,
            };

            let child_index = Nibble::try_from(u8::try_from(child_index).unwrap()).unwrap();
            child.parent = Some((new_branch_node_index, child_index));
            truncate_first_elems(&mut child.partial_key, new_branch_node_partial_key_len + 1);
        }

        // Update the branch node's parent to point to its new child.
        if let Some((parent_index, child_index)) = self.branch_parent {
            let mut parent = self.trie.nodes.get_mut(parent_index).unwrap();
            parent.children[usize::from(u8::from(child_index))] = Some(new_branch_node_index);
        } else {
            self.trie.root_index = Some(new_branch_node_index);
        }

        // Success!
        StorageNodeAccess {
            trie: self.trie,
            node_index: new_storage_node_index,
        }
    }
}

/// Inserts `first` and `second` at the beginning of `vec`.
fn insert_front(vec: &mut Vec<Nibble>, first: Vec<Nibble>, next: Nibble) {
    let shift = first.len() + 1;
    let previous_len = vec.len();
    vec.resize(vec.len() + shift, Nibble::try_from(0).unwrap());
    for n in (0..previous_len).rev() {
        vec[n + shift] = vec[n];
    }
    vec[0..first.len()].copy_from_slice(&first);
    vec[first.len()] = next;
}

/// Removes the first `num` elements of `vec`.
fn truncate_first_elems(vec: &mut Vec<Nibble>, num: usize) {
    debug_assert!(num <= vec.len());
    for n in num..vec.len() {
        vec[n - num] = vec[n];
    }
    vec.truncate(vec.len() - num);
}
