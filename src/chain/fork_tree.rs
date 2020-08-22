//! Data structure containing trees of nodes.
//!
//! This data structure is meant to be used in situations where there exists a finalized block,
//! plus a tree of non-finalized nodes that all descend from that finalized block.
//!
//! In this schema, the finalized block is **not** part of the `ForkTree` data structure. Only
//! its descendants are.

use core::{fmt, iter};

/// Tree of nodes. Each node contains a value of type `T`.
pub struct ForkTree<T> {
    /// Container storing the nodes.
    nodes: slab::Slab<Node<T>>,
    /// Index of the node in the tree without any root.
    first_root: Option<usize>,
}

struct Node<T> {
    parent: Option<usize>,
    first_child: Option<usize>,
    next_sibling: Option<usize>,
    previous_sibling: Option<usize>,
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

    /// Shrink the capacity of the tree as much as possible.
    pub fn shrink_to_fit(&mut self) {
        self.nodes.shrink_to_fit()
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
    /// # Panic
    ///
    /// Panics if the [`NodeIndex`] is invalid.
    ///
    pub fn prune_ancestors(&mut self, node_index: NodeIndex) {
        // The implementation consists in ultimately replacing the content of `self.first_root`
        // with the content of `self.nodes[node_index].first_child` and updating everything else
        // accordingly. Save the value here for later.
        let new_first_root = self.nodes[node_index.0].first_child;

        // Traverse all the nodes, starting from the root, and removing them one by one.
        let mut iter = self.first_root.unwrap();
        let mut traversing_up = false;
        loop {
            let mut iter_node = &mut self.nodes[iter];

            // If current node is a direct child of `node_index`, then don't remove it.
            // Instead, just update its parent to be `None` and continue iterating.
            if iter_node.parent == Some(node_index.0) {
                debug_assert!(!traversing_up);
                iter_node.parent = None;
                iter = if let Some(next_sibling) = iter_node.next_sibling {
                    next_sibling
                } else {
                    traversing_up = true;
                    node_index.0
                };
                continue;
            }

            // If `traversing_up` is false`, try to go down the hierarchy as deeply as possible.
            if !traversing_up {
                if let Some(first_child) = self.nodes[iter].first_child {
                    iter = first_child;
                    continue;
                }
            }

            // Remove node, then jump either to its next sibling, or, if it was the last sibling,
            // back to its parent.
            let iter_node = self.nodes.remove(iter);
            iter = if let Some(next_sibling) = iter_node.next_sibling {
                traversing_up = false;
                next_sibling
            } else if let Some(parent) = iter_node.parent {
                traversing_up = true;
                parent
            } else {
                break;
            };
        }

        debug_assert!(!self.nodes.contains(node_index.0));
        self.first_root = new_first_root;
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
            let next_sibling = self.nodes.get_mut(parent.0).unwrap().first_child.clone();

            let new_node_index = self.nodes.insert(Node {
                parent: Some(parent.0),
                first_child: None,
                next_sibling,
                previous_sibling: None,
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

        tree.prune_ancestors(node1);
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
}
