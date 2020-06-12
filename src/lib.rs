// TODO: fix all the compilation errors caused by the copy-pasted code
//#![cfg_attr(not(test), no_std)]
#![recursion_limit = "512"]

extern crate alloc;

pub mod block;
pub mod block_import;
pub mod chain_spec;
pub mod database;
pub mod executor;
pub mod informant;
pub mod keystore;
pub mod network;
pub mod service;
//pub mod storage_cache;
pub mod trie;

/// Calculates the hash of the genesis block from the storage.
///
/// # Context
///
/// A blockchain is a key-value database. Each block built at the head of the chain updates
/// entries in this key-value database.
///
/// In order to make things easier, there exists a special block whose number is 0 and that
/// is called the genesis block. Block number 1 while have as parent the genesis block (then,
/// block number 2 has block number 1 as parent, and so on).
///
/// The hash of the genesis block depends purely on the initial state of the content.
/// This function makes it possible to calculate this hash.
pub fn calculate_genesis_block_hash<'a>(
    genesis_storage: impl Iterator<Item = (&'a [u8], &'a [u8])>,
) -> [u8; 32] {
    let mut state_trie = trie::Trie::new();
    for (key, value) in genesis_storage {
        state_trie.insert(key, value.to_vec());
    }

    let genesis_block_header = block::Header {
        parent_hash: [0; 32].into(),
        number: 0,
        state_root: state_trie.root_merkle_value(None).into(),
        extrinsics_root: trie::Trie::new().root_merkle_value(None).into(),
        digest: block::Digest { logs: Vec::new() },
    };

    genesis_block_header.block_hash().0
}
