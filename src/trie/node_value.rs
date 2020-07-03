//! Calculation of the Merkle value of a node given the information about it.

use arrayvec::ArrayVec;
use core::{convert::TryFrom as _, fmt};
use parity_scale_codec::Encode as _;

/// Information about a node whose Merkle value is to be calculated.
///
/// The documentation here assumes that you already know how the trie works.
pub struct Config<'a, TPKey, TVal> {
    /// True if this is the root node of the trie.
    pub is_root: bool,

    /// Merkle values of the 16 possible children of the node. `None` if there is no child at this
    /// index.
    pub children: &'a [Option<Output>],

    /// Partial key of the node, as an iterator of nibbles.
    ///
    /// The key of each node is made of three parts:
    ///
    /// - The parent's key.
    /// - The child index, one single nibble indicating which child we are relative to the parent.
    /// - The partial key.
    pub partial_key: TPKey,

    /// Value of the node in the storage.
    pub stored_value: Option<TVal>,
}

/// Calculates the Merkle value of a node given the information about this node.
///
/// # Panic
///
/// Panics if `config.children.len() != 16`.
/// Panics if any entry in `partial_key` is >= 16.
///
pub fn calculate_merke_root<TPKey, TVal>(mut config: Config<TPKey, TVal>) -> Output
where 
    TPKey: ExactSizeIterator<Item = u8>,
    TVal: AsRef<[u8]>,
{
    assert_eq!(config.children.len(), 16);

    let has_children = config.children.iter().any(|c| c.is_some());

    // This value will be used as the sink for all the components of the merkle value.
    let mut merkle_value_sink = if config.is_root {
        HashOrInline::Hasher(blake2_rfc::blake2b::Blake2b::new(32))
    } else {
        HashOrInline::Inline(ArrayVec::new())
    };

    // Push the header of the node to `merkle_value_sink`.
    {
        // The first two most significant bits of the header contain the type of node.
        let two_msb: u8 = {
            let has_stored_value = config.stored_value.is_some();
            match (has_stored_value, has_children) {
                (false, false) => {
                    // This should only ever be reached if we compute the root node of an
                    // empty trie.
                    0b00
                }
                (true, false) => 0b01,
                (false, true) => 0b10,
                (true, true) => 0b11,
            }
        };

        // Another weird algorithm to encode the partial key length into the header.
        let mut pk_len = config.partial_key.len();
        if pk_len >= 63 {
            pk_len -= 63;
            merkle_value_sink.update(&[(two_msb << 6) + 63]);
            while pk_len > 255 {
                pk_len -= 255;
                merkle_value_sink.update(&[255]);
            }
            merkle_value_sink.update(&[u8::try_from(pk_len).unwrap()]);
        } else {
            merkle_value_sink.update(&[(two_msb << 6) + u8::try_from(pk_len).unwrap()]);
        }
    }

    // Turn the `config.partial_key` into bytes with a weird encoding and push it to
    // `merkle_value_sink`.
    if config.partial_key.len() % 2 != 0 {
        // next().unwrap() can't panic, otherwise `len() % 2` would have returned 0.
        merkle_value_sink.update(&[config.partial_key.next().unwrap()]);
    }
    {
        let mut previous = None;
        for nibble in config.partial_key {
            assert!(nibble < 16);
            if let Some(prev) = previous.take() {
                let val = (prev << 4) | nibble;
                merkle_value_sink.update(&[val]);
            } else {
                previous = Some(nibble);
            }
        }
        assert!(previous.is_none());
    }

    // Compute the node subvalue and push it to `merkle_value_sink`.

    // If there isn't any children, the node subvalue only consists in the storage value.
    // We take a shortcut and end the calculation now.
    if !has_children {
        if let Some(stored_value) = config.stored_value {
            // Doing something like `merkle_value_sink.update(stored_value.encode());` would be
            // quite expensive because we would duplicate the storage value. Instead, we do the
            // encoding manually by pushing the length then the value.
            parity_scale_codec::Compact(u64::try_from(stored_value.as_ref().len()).unwrap())
                .encode_to(&mut merkle_value_sink);
            merkle_value_sink.update(stored_value.as_ref());
        }

        return merkle_value_sink.finalize();
    }

    // If there is any child, we a `u16` where each bit is `1` if there exists a child there.
    merkle_value_sink.update({
        let mut children_bitmap = 0u16;
        for (child_index, child) in config.children.iter().enumerate() {
            if child.is_some() {
                children_bitmap |= 1 << u32::try_from(child_index).unwrap();
            }
        }
        &children_bitmap.to_le_bytes()[..]
    });

    // Push the merkle values of all the children.
    for child in config.children.iter() {
        let child_merkle_value = match child {
            Some(v) => v,
            None => continue,
        };

        // Doing something like `merkle_value_sink.update(child_merkle_value.encode());` would be
        // expensive because we would duplicate the merkle value. Instead, we do the encoding
        // manually by pushing the length then the value.
            parity_scale_codec::Compact(u64::try_from(child_merkle_value.as_ref().len()).unwrap())
            .encode_to(&mut merkle_value_sink);
        merkle_value_sink.update(child_merkle_value.as_ref());
    }

    // Finally, add our own stored value.
    if let Some(stored_value) = config.stored_value {
        // Doing something like `merkle_value_sink.update(stored_value.encode());` would be
        // quite expensive because we would duplicate the storage value. Instead, we do the
        // encoding manually by pushing the length then the value.
        parity_scale_codec::Compact(u64::try_from(stored_value.as_ref().len()).unwrap())
            .encode_to(&mut merkle_value_sink);
        merkle_value_sink.update(stored_value.as_ref());
    }

    merkle_value_sink.finalize()
}

/// Output of the calculation.
#[derive(Clone)]
pub struct Output {
    inner: OutputInner,
}

#[derive(Clone)]
enum OutputInner {
    Inline(ArrayVec<[u8; 31]>),
    Hasher(blake2_rfc::blake2b::Blake2bResult),
    Bytes(ArrayVec<[u8; 32]>),
}

impl Output {
    /// Builds an [`Output`] from a slice of bytes.
    ///
    /// # Panic
    ///
    /// Panics if `bytes.len() > 32`.
    ///
    pub fn from_bytes(bytes: &[u8]) -> Output {
        assert!(bytes.len() <= 32);
        Output {
            inner: OutputInner::Bytes({
                let mut v = ArrayVec::new();
                v.try_extend_from_slice(bytes).unwrap();
                v
            })
        }
    }
}

impl AsRef<[u8]> for Output {
    fn as_ref(&self) -> &[u8] {
        match &self.inner {
            OutputInner::Inline(a) => a.as_slice(),
            OutputInner::Hasher(a) => a.as_bytes(),
            OutputInner::Bytes(a) => a.as_slice(),
        }
    }
}

impl fmt::Debug for Output {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self.as_ref(), f)
    }
}

/// The merkle value of a node is defined as either the hash of the node value, or the node value
/// itself if it is shorted than 32 bytes (or if we are the root).
///
/// This struct serves as a helper to handle these situations. Rather than putting intermediary
/// values in buffers then hashing the node value as a whole, we push the elements of the node
/// value to this struct which automatically switches to hashing if the value exceeds 32 bytes.
enum HashOrInline {
    Inline(ArrayVec<[u8; 31]>),
    Hasher(blake2_rfc::blake2b::Blake2b),
}

impl HashOrInline {
    /// Adds data to the node value. If this is a [`HashOrInline::Inline`] and the total size would
    /// go above 32 bytes, then we switch to a hasher.
    fn update(&mut self, data: &[u8]) {
        match self {
            HashOrInline::Inline(curr) => {
                if curr.try_extend_from_slice(data).is_err() {
                    let mut hasher = blake2_rfc::blake2b::Blake2b::new(32);
                    hasher.update(&curr);
                    hasher.update(data);
                    *self = HashOrInline::Hasher(hasher);
                }
            }
            HashOrInline::Hasher(hasher) => {
                hasher.update(data);
            }
        }
    }

    fn finalize(self) -> Output {
        Output {
            inner: match self {
                HashOrInline::Inline(b) => OutputInner::Inline(b),
                HashOrInline::Hasher(h) => OutputInner::Hasher(h.finalize()),
            }
        }
    }
}

impl parity_scale_codec::Output for HashOrInline {
    fn write(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }
}
