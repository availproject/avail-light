//! Data structure containing all blocks in the chain.

use parity_scale_codec::Decode as _;

pub struct Database {
    inner: Option<parity_db::Db>,
}

/// Columns indices in the key-value store.
///
/// Copy-pasted from upstream Substrate code.
mod columns {
    pub const META: u8 = 0;
    pub const STATE: u8 = 1;
    pub const STATE_META: u8 = 2;
    pub const KEY_LOOKUP: u8 = 3;
    pub const HEADER: u8 = 4;
    pub const BODY: u8 = 5;
    pub const JUSTIFICATION: u8 = 6;
    pub const CHANGES_TRIE: u8 = 7;
    pub const AUX: u8 = 8;
    pub const OFFCHAIN: u8 = 9;
    pub const CACHE: u8 = 10;
}

impl Database {
    pub fn open() -> Self {
        // TODO:
        let inner = {
            let mut options = parity_db::Options::with_columns(
                std::path::Path::new(
                    "/home/pierre/.local/share/substrate/chains/flamingfir7/paritydb",
                ),
                11,
            );
            let mut column_options = &mut options.columns[usize::from(columns::STATE)];
            column_options.ref_counted = true;
            column_options.preimage = true;
            column_options.uniform = true;
            parity_db::Db::open(&options).unwrap()
        };

        let genesis_hash = inner.get(columns::META, b"gen").unwrap().unwrap();
        println!("genesis hash = {:?}", genesis_hash);

        let best_block_lookup = inner.get(columns::META, b"best").unwrap().unwrap();
        let best_block = inner
            .get(columns::HEADER, &best_block_lookup)
            .unwrap()
            .unwrap();
        println!("best block = {:?}", best_block);
        let decoded = crate::block::Header::decode(&mut &best_block[..]).unwrap();
        println!("decoded block = {:?}", decoded);

        Database { inner: Some(inner) }
    }
}
