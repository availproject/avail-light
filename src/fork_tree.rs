//! Data structure containing trees of nodes.
//!
//! This data structure is meant to be used in situations where there exists a finalized block,
//! plus a tree of non-finalized nodes that all descend from that finalized block.
//!
//! In this schema, the finalized block is **not** part of the `ForkTree` data structure. Only
//! its descendants are.

use core::iter;

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
    /// - Any node without any parent.
    ///
    pub fn prune_ancestors(&mut self, node_index: NodeIndex) {
        todo!()
    }

    /// Returns all the nodes, starting from the the given node, to the root. Each element
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

/// Index of a node within a [`ForkTree`]. Never invalidated unless the node is removed.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeIndex(usize);
