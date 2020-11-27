// Substrate-lite
// Copyright (C) 2019-2020  Parity Technologies (UK) Ltd.
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

//! Data structure containing trees of nodes.
//!
//! The [`ForkTree`] data structure can be used in situations where there exists a finalized
//! block, plus a tree of non-finalized blocks that all descend from that finalized block.
//!
//! The [`ForkTree`] only contains the non-finalized blocks. The finalized block, virtual root
//! of the tree, is only implicitly there.
//!
//! # Example
//!
//! ```
//! use substrate_lite::chain::fork_tree::ForkTree;
//!
//! let mut tree = ForkTree::new();
//!
//! // Add a first node with no parent. In other words, its parent is the finalized block that is
//! // virtually there but not managed by the `ForkTree`.
//! // Note that the user data (`"foo"` here) can be of any type. It can be used to store
//! // additional information on each node.
//! let node0 = tree.insert(None, "foo");
//! // Add a second node, child of the first one.
//! let node1 = tree.insert(Some(node0), "bar");
//! // Add a third node, child of the second one.
//! let node2 = tree.insert(Some(node1), "baz");
//!
//! // Removes `node1` and all the nodes that aren't its descendants.
//! // This is typically called when `node1` gets finalized.
//! // This function returns an iterator containing the blocks that have been removed.
//! tree.prune_ancestors(node1);
//!
//! // Only `node2` remains.
//! assert!(tree.get(node0).is_none());
//! assert!(tree.get(node1).is_none());
//! assert!(tree.get(node2).is_some());
//! ```

use core::{fmt, iter};

/// Tree of nodes. Each node contains a value of type `T`.
pub struct ForkTree<T> {
    /// Container storing the nodes.
    nodes: slab::Slab<Node<T>>,
    /// Index of the node in the tree without any parent nor previous sibling.
    first_root: Option<usize>,
}

struct Node<T> {
    /// Index within [`ForkTree::nodes`] of the parent of that node. `None` if the node is a root.
    parent: Option<usize>,
    /// Index within [`ForkTree::nodes`] of the first child of that node. `None` if no children.
    first_child: Option<usize>,
    /// Index within [`ForkTree::nodes`] of the next sibling of that node. `None` if that node is
    /// the last child of its parent.
    next_sibling: Option<usize>,
    /// Index within [`ForkTree::nodes`] of the previous sibling of that node. `None` if the node
    /// is the first child of its parent.
    previous_sibling: Option<usize>,
    /// Always `false`, except temporarily set to `true` during the pruning process on nodes that
    /// are ancestors of the pruning target.
    is_prune_target_ancestor: bool,
    /// Data decided by the external API user.
    data: T,
}

impl<T> ForkTree<T> {
    /// Initializes a new `ForkTree`.
    pub fn new() -> Self {
        ForkTree {
            nodes: slab::Slab::new(),
            first_root: None,
        }
    }

    /// Initializes a new `ForkTree` with a certain pre-allocated capacity.
    pub fn with_capacity(cap: usize) -> Self {
        ForkTree {
            nodes: slab::Slab::with_capacity(cap),
            first_root: None,
        }
    }

    /// Reserves additional capacity for at least `additional` new blocks without allocating.
    pub fn reserve(&mut self, additional: usize) {
        self.nodes.reserve(additional)
    }

    /// Removes all elements in the tree, leaving it empty.
    pub fn clear(&mut self) {
        self.nodes.clear();
    }

    /// Shrink the capacity of the tree as much as possible.
    pub fn shrink_to_fit(&mut self) {
        self.nodes.shrink_to_fit()
    }

    /// Returns true if there isn't any element in the tree.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns the number of elements in the tree.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns an iterator to all the node values without any specific order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.nodes.iter().map(|n| &n.1.data)
    }

    /// Returns the value of the node with the given index.
    pub fn get(&self, index: NodeIndex) -> Option<&T> {
        self.nodes.get(index.0).map(|n| &n.data)
    }

    /// Returns the value of the node with the given index.
    pub fn get_mut(&mut self, index: NodeIndex) -> Option<&mut T> {
        self.nodes.get_mut(index.0).map(|n| &mut n.data)
    }

    /// Removes from the tree:
    ///
    /// - The node passed as parameter.
    /// - The ancestors of the node passed as parameter.
    /// - Any node not a descendant of the node passed as parameter.
    ///
    /// Returns an iterator containing the removed elements.
    /// All elements are removed from the tree even if the returned iterator is dropped eagerly.
    ///
    /// Elements are returned in an unspecified order. However, all the elements for which
    /// [`PrunedNode::is_prune_target_ancestor`] is `true` are guaranteed to be returned from
    /// child to parent.
    ///
    /// In other words, doing `prune_ancestors(...).filter(|n| n.is_prune_target_ancestor)` is
    /// guaranteed to return the elements in the same order as [`ForkTree::node_to_root_path`]
    /// does.
    ///
    /// # Panic
    ///
    /// Panics if the [`NodeIndex`] is invalid.
    ///
    pub fn prune_ancestors(&mut self, node_index: NodeIndex) -> PruneAncestorsIter<T> {
        // The implementation consists in replacing the content of `self.first_root` with the
        // content of `self.nodes[node_index].first_child` and updating everything else
        // accordingly.

        // Set `is_prune_target_ancestor` to true for `node_index` and all its ancestors.
        {
            let mut node = node_index.0;
            loop {
                debug_assert!(!self.nodes[node].is_prune_target_ancestor);
                self.nodes[node].is_prune_target_ancestor = true;
                node = match self.nodes[node].parent {
                    Some(n) => n,
                    None => break,
                }
            }
        }

        let iter = self.first_root.unwrap();
        self.first_root = self.nodes[node_index.0].first_child;

        PruneAncestorsIter {
            finished: false,
            tree: self,
            new_final: node_index,
            iter,
            traversing_up: false,
        }
    }

    /// Returns the common ancestor between `node1` and `node2`, if any is known.
    ///
    /// # Panic
    ///
    /// Panics if one of the [`NodeIndex`]s is invalid.
    ///
    pub fn common_ancestor(&self, node1: NodeIndex, node2: NodeIndex) -> Option<NodeIndex> {
        let dist_to_root1 = self.node_to_root_path(node1).count();
        let dist_to_root2 = self.node_to_root_path(node2).count();

        let mut iter1 = self
            .node_to_root_path(node1)
            .skip(dist_to_root1.saturating_sub(dist_to_root2));
        let mut iter2 = self
            .node_to_root_path(node2)
            .skip(dist_to_root2.saturating_sub(dist_to_root1));

        loop {
            match (iter1.next(), iter2.next()) {
                (Some(a), Some(b)) if a == b => return Some(a),
                (Some(_), Some(_)) => continue,
                (None, None) => return None,
                _ => unreachable!(),
            }
        }
    }

    /// Returns true if `maybe_ancestor` is an ancestor of `maybe_descendant`. Also returns `true`
    /// if the two [`NodeIndex`] are equal.
    ///
    /// # Panic
    ///
    /// Panics if one of the [`NodeIndex`]s is invalid.
    ///
    pub fn is_ancestor(&self, maybe_ancestor: NodeIndex, maybe_descendant: NodeIndex) -> bool {
        // Do this check separately, otherwise the code below would successfully return `true`
        // if `maybe_ancestor` and `descendant` are invalid but equal.
        assert!(self.nodes.contains(maybe_descendant.0));

        let mut iter = maybe_descendant.0;
        loop {
            if iter == maybe_ancestor.0 {
                return true;
            }

            iter = match self.nodes[iter].parent {
                Some(p) => p,
                None => return false,
            };
        }
    }

    /// Returns two iterators: the first iterator enumerates the nodes from `node1` to the common
    /// ancestor of `node1` and `node2`. The second iterator enumerates the nodes from that common
    /// ancestor to `node2`. The common ancestor isn't included in either iterator. If `node1` and
    /// `node2` are equal then both iterators are empty, otherwise `node1` is always included in
    /// the first iterator and `node2` always included in the second iterator.
    ///
    /// # Panic
    ///
    /// Panics if one of the [`NodeIndex`]s is invalid.
    ///
    pub fn ascend_and_descend<'a>(
        &'a self,
        node1: NodeIndex,
        node2: NodeIndex,
    ) -> (
        impl Iterator<Item = NodeIndex> + 'a,
        impl Iterator<Item = NodeIndex> + 'a,
    ) {
        let common_ancestor = self.common_ancestor(node1, node2);

        let iter1 = self
            .node_to_root_path(node1)
            .take_while(move |v| Some(*v) != common_ancestor);
        let iter2 = self
            .node_to_root_path(node2)
            .take_while(move |v| Some(*v) != common_ancestor);

        (iter1, iter2)
    }

    /// Enumerates all the nodes, starting from the the given node, to the root. Each element
    /// returned by the iterator is a parent of the previous one. The iterator does include the
    /// node itself.
    ///
    /// # Panic
    ///
    /// Panics if the [`NodeIndex`] is invalid.
    ///
    pub fn node_to_root_path<'a>(
        &'a self,
        node_index: NodeIndex,
    ) -> impl Iterator<Item = NodeIndex> + 'a {
        iter::successors(Some(node_index), move |n| {
            self.nodes[n.0].parent.map(NodeIndex)
        })
    }

    /// Same as [`ForkTree::node_to_root_path`], but gives the path in the reverse order.
    ///
    /// # Panic
    ///
    /// Panics if the [`NodeIndex`] is invalid.
    ///
    pub fn root_to_node_path<'a>(
        &'a self,
        node_index: NodeIndex,
    ) -> impl Iterator<Item = NodeIndex> + 'a {
        debug_assert!(self.nodes.get(usize::max_value()).is_none());

        // First element is an invalid key, each successor is the last element of
        // `node_to_root_path(node_index)` that isn't equal to `current`.
        // Since the first element is invalid, we skip it.
        iter::successors(Some(NodeIndex(usize::max_value())), move |&current| {
            self.node_to_root_path(node_index)
                .take_while(move |n| *n != current)
                .last()
        })
        .skip(1)
    }

    /// Finds the first node in the tree that matches the given condition.
    pub fn find(&self, mut cond: impl FnMut(&T) -> bool) -> Option<NodeIndex> {
        self.nodes
            .iter()
            .filter(|(_, n)| cond(&n.data))
            .map(|(i, _)| i)
            .next()
            .map(NodeIndex)
    }

    /// Inserts a new node in the tree.
    ///
    /// # Panic
    ///
    /// Panics if `parent` isn't a valid node index.
    ///
    pub fn insert(&mut self, parent: Option<NodeIndex>, child: T) -> NodeIndex {
        if let Some(parent) = parent {
            let next_sibling = self.nodes.get_mut(parent.0).unwrap().first_child;

            let new_node_index = self.nodes.insert(Node {
                parent: Some(parent.0),
                first_child: None,
                next_sibling,
                previous_sibling: None,
                is_prune_target_ancestor: false,
                data: child,
            });

            self.nodes.get_mut(parent.0).unwrap().first_child = Some(new_node_index);

            if let Some(next_sibling) = next_sibling {
                self.nodes.get_mut(next_sibling).unwrap().previous_sibling = Some(new_node_index);
            }

            NodeIndex(new_node_index)
        } else {
            let new_node_index = self.nodes.insert(Node {
                parent: None,
                first_child: None,
                next_sibling: self.first_root,
                previous_sibling: None,
                is_prune_target_ancestor: false,
                data: child,
            });

            if let Some(first_root) = self.first_root {
                self.nodes.get_mut(first_root).unwrap().previous_sibling = Some(new_node_index);
            }

            self.first_root = Some(new_node_index);

            NodeIndex(new_node_index)
        }
    }
}

impl<T> Default for ForkTree<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for ForkTree<T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list()
            .entries(self.nodes.iter().map(|(_, v)| &v.data))
            .finish()
    }
}

/// Iterator of elements removed when pruning ancestors.
pub struct PruneAncestorsIter<'a, T> {
    /// True if iterator is completed. If true, none of the values of the other fields are relevant
    /// anymore.
    finished: bool,

    // Parent object.
    // Note that `first_root` has already been updated to be the new final, and therefore
    // shouldn't be relied upon.
    tree: &'a mut ForkTree<T>,

    /// Current node being iterated.
    /// Order of iteration is: go down the hierarchy as deep as possible by following the first
    /// child. If there is no first child, instead go to the next sibling. If there is no next
    /// sibling, go to the parent.
    iter: usize,

    /// True if the previous iteration was from a node lower in the hierarchy.
    traversing_up: bool,

    /// Target of the pruning. Value which `prune_ancestors` has been called with.
    new_final: NodeIndex,
}

impl<'a, T> Iterator for PruneAncestorsIter<'a, T> {
    type Item = PrunedNode<T>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.finished {
                break None;
            }

            let mut iter_node = &mut self.tree.nodes[self.iter];

            // If current node is a direct child of `new_final`, then don't remove it.
            // Instead, just update its parent to be `None` and continue iterating.
            if iter_node.parent == Some(self.new_final.0) {
                debug_assert!(!self.traversing_up);
                iter_node.parent = None;
                self.iter = if let Some(next_sibling) = iter_node.next_sibling {
                    next_sibling
                } else {
                    self.traversing_up = true;
                    self.new_final.0
                };
                continue;
            }

            // If `traversing_up` is false`, try to go down the hierarchy as deeply as possible.
            if !self.traversing_up {
                if let Some(first_child) = self.tree.nodes[self.iter].first_child {
                    self.iter = first_child;
                    continue;
                }
            }

            // Remove node.
            let removed_node_index = NodeIndex(self.iter);
            let iter_node = self.tree.nodes.remove(self.iter);

            // Jump either to its next sibling, or, if it was the last sibling, back to its
            // parent.
            if let Some(next_sibling) = iter_node.next_sibling {
                self.traversing_up = false;
                self.iter = next_sibling;
            } else if let Some(parent) = iter_node.parent {
                self.traversing_up = true;
                self.iter = parent;
            } else {
                self.finished = true;
            };

            break Some(PrunedNode {
                index: removed_node_index,
                is_prune_target_ancestor: iter_node.is_prune_target_ancestor,
                user_data: iter_node.data,
            });
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.tree.nodes.len()))
    }
}

impl<'a, T> Drop for PruneAncestorsIter<'a, T> {
    fn drop(&mut self) {
        // Make sure that all elements are removed.
        while let Some(_) = self.next() {}

        debug_assert!(self.tree.get(self.new_final).is_none());
    }
}

/// Node removed by [`ForkTree::prune_ancestors`].
pub struct PrunedNode<T> {
    /// Former index of the node. This index is no longer valid.
    pub index: NodeIndex,
    /// True if this node is an ancestor of the target of the pruning.
    pub is_prune_target_ancestor: bool,
    /// Value that was passed to [`ForkTree::insert`].
    pub user_data: T,
}

/// Index of a node within a [`ForkTree`]. Never invalidated unless the node is removed.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeIndex(usize);

#[cfg(test)]
mod tests {
    use super::ForkTree;

    #[test]
    fn basic() {
        let mut tree = ForkTree::new();

        let node0 = tree.insert(None, 0);
        let node1 = tree.insert(Some(node0), 1);
        let node2 = tree.insert(Some(node1), 2);
        let node3 = tree.insert(Some(node2), 3);
        let node4 = tree.insert(Some(node2), 4);
        let node5 = tree.insert(Some(node0), 5);

        assert_eq!(tree.find(|v| *v == 0), Some(node0));
        assert_eq!(tree.find(|v| *v == 1), Some(node1));
        assert_eq!(tree.find(|v| *v == 2), Some(node2));
        assert_eq!(tree.find(|v| *v == 3), Some(node3));
        assert_eq!(tree.find(|v| *v == 4), Some(node4));
        assert_eq!(tree.find(|v| *v == 5), Some(node5));

        assert_eq!(
            tree.node_to_root_path(node3).collect::<Vec<_>>(),
            &[node3, node2, node1, node0]
        );
        assert_eq!(
            tree.node_to_root_path(node4).collect::<Vec<_>>(),
            &[node4, node2, node1, node0]
        );
        assert_eq!(
            tree.node_to_root_path(node1).collect::<Vec<_>>(),
            &[node1, node0]
        );
        assert_eq!(
            tree.node_to_root_path(node5).collect::<Vec<_>>(),
            &[node5, node0]
        );

        let iter = tree.prune_ancestors(node1);
        assert_eq!(
            iter.filter(|n| n.is_prune_target_ancestor)
                .map(|n| n.index)
                .collect::<Vec<_>>(),
            vec![node1, node0]
        );

        assert!(tree.get(node0).is_none());
        assert!(tree.get(node1).is_none());
        assert_eq!(tree.get(node2), Some(&2));
        assert_eq!(tree.get(node3), Some(&3));
        assert_eq!(tree.get(node4), Some(&4));
        assert!(tree.get(node5).is_none());

        assert_eq!(
            tree.node_to_root_path(node3).collect::<Vec<_>>(),
            &[node3, node2]
        );
        assert_eq!(
            tree.node_to_root_path(node4).collect::<Vec<_>>(),
            &[node4, node2]
        );
    }

    // TODO: add more testing for the order of elements returned by `prune_ancestors`
}
