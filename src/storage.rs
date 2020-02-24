//! Storage of abstract data, accessible from the runtime.

use fnv::FnvBuildHasher;
use hashbrown::HashMap;

// TODO: document and/or rewrite all these structs

/// Map of data to use in a storage, it is a collection of
/// byte key and values.
pub type StorageMap = HashMap<Vec<u8>, Vec<u8>, FnvBuildHasher>;

/// Child trie storage data.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct StorageChild {
	/// Child data for storage.
	pub data: StorageMap,
	/// Associated child info for a child trie.
	pub child_info: OwnedChildInfo,
}

/// Struct containing data needed for a storage.
#[derive(Default, Debug, Clone)]
pub struct Storage {
	/// Top trie storage data.
	pub top: StorageMap,
	/// Children trie storage data by storage key.
	pub children: HashMap<Vec<u8>, StorageChild, FnvBuildHasher>,
}

/// Owned version of `ChildInfo`.
/// To be use in persistence layers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OwnedChildInfo {
	Default(OwnedChildTrie),
}

/// Owned version of default child trie `ChildTrie`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OwnedChildTrie {
	data: Vec<u8>,
}
