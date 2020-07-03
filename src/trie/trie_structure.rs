use arrayvec::ArrayVec;
use fnv::FnvBuildHasher;
use hashbrown::HashMap;
use slab::Slab;

/// Stores the structure of a trie, including branch nodes that have no storage value.
///
/// The `TUd` parameter is a user data stored in each node.
///
/// This struct doesn't represent a complete trie. It only manages the structure of the trie, and
/// the storage values have to be maintained in parallel of this.
#[derive(Debug)]
pub struct TrieStructure<TUd> {
    /// List of nodes. Using a [`Slab`] guarantees that the node indices don't change.
    nodes: Slab<Node>,
    /// For each node that exists, associates the list of nibbles that compose its key to its
    /// index within [`nodes`].
    index_by_key: HashMap<Vec<u8>, usize>,
}

#[derive(Debug)]
struct Node<TUd> {
    /// Index of the parent within [`TrieStructure::nodes`]. `None` if this is the root.
    parent: Option<(usize, u8)>,
    partial_key: Vec<u8>, // TODO: SmallVec?
    /// Indices of the children within [`TrieStructure::nodes`].
    children: [Option<usize>; 16],
    has_storage_value: bool,
    /// User data associated to the node.
    user_data: TUd,
}

impl TrieStructure {
    pub fn empty(root_node_user_data: TUd) {

    }

    pub fn node<'a>(&'a mut self, key: &'a [u8]) -> NodeAccess<'a, TUd> {
        match self.nodes.get()
    }
}

/// Access to a entry for a potential node within the [`TrieStructure`].
pub enum Entry<'a, TUd> {
    Occupied(NodeAccess<'a, TUd>),
    Vacant(Vacant<'a, TUd>),
}

/// Access to a node within the [`TrieStructure`].
pub enum NodeAccess<'a, TUd> {
    Storage(StorageNodeAccess<'a, TUd>),
    Branch(BranchNodeAccess<'a, TUd>),
}

impl<'a, TUd> NodeAccess<'a, TUd> {
    /// Returns true if the node has a storage value associated to it.
    pub fn has_storage_value(&self) -> bool {
        match self {
            NodeAccess::Storage(_) => true,
            NodeAccess::Branch(_) => false,
            NodeAccess::Vacant(_) => false,
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
    /// Returns the user data associated to this node.
    pub fn user_data(&mut self) -> &mut TUd {
        &mut self.trie.nodes.get_mut(self.node_index).unwrap().user_data
    }

    /// Removes the storage value and returns what this changes in the trie structure.
    pub fn remove(self) -> Remove<'a, TUd> {

    }
}

pub enum Remove<'a, TUd> {
    /// Removing the storage value didn't change the structure of the trie. Contains a
    /// [`BranchNodeAccess`] representing the same node as the [`StorageNodeAccess`] whose value
    /// got removed.
    NowBranch(BranchNodeAccess<'a, TUd>),
}

/// Access to a node within the [`TrieStructure`] that is known to not have any storage value
/// associated to it.
pub struct BranchNodeAccess<'a, TUd> {
    trie: &'a mut TrieStructure<TUd>,
    node_index: usize,
}

impl<'a, TUd> BranchNodeAccess<'a, TUd> {
    /// Adds a storage value to this node, turning it into a [`StorageNodeAccess`].
    ///
    /// The trie structure doesn't change.
    pub fn insert_storage_value(self) -> StorageNodeAccess<'a, TUd> {
        &mut self.trie.nodes.get_mut(self.node_index).unwrap().has_storage_value = true;

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
pub struct Vacant<'a, TUd> {
    trie: &'a mut TrieStructure<TUd>,
    key: &'a [u8],
}

impl<'a, TUd> BranchNodeAccess<'a, TUd> {
    /// Creates the node in question.
    pub fn add(&mut self) -> &mut TUd {
        &mut self.trie.nodes.get_mut(self.node_index).unwrap().user_data
    }
}
