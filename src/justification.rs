//! Justifications contain a proof of the finality of a block.

use core::{convert::TryFrom, fmt};

/// Attempt to decode the given SCALE-encoded justification.
pub fn decode<'a>(mut scale_encoded: &'a [u8]) -> Result<JustificationRef<'a>, Error> {
    if scale_encoded.len() < 8 + 32 + 4 + 1 + 1 {
        return Err(Error::TooShort);
    }

    let round = u64::from_le_bytes(<[u8; 8]>::try_from(&scale_encoded[..8]).unwrap());
    scale_encoded = &scale_encoded[8..];

    let target_hash: &[u8; 32] = TryFrom::try_from(&scale_encoded[..32]).unwrap();
    scale_encoded = &scale_encoded[32..];

    let target_number = u32::from_le_bytes(<[u8; 4]>::try_from(&scale_encoded[..4]).unwrap());
    scale_encoded = &scale_encoded[4..];

    let num_precommits: parity_scale_codec::Compact<u64> =
        parity_scale_codec::Decode::decode(&mut scale_encoded)
            .map_err(Error::NumElementsDecodeError)?;

    let precommits_len =
        usize::try_from(num_precommits.0).map_err(|_| Error::TooShort)? * PRECOMMIT_ENCODED_LEN;
    if scale_encoded.len() < precommits_len + 1 {
        return Err(Error::TooShort);
    }

    let precommits_data = &scale_encoded[..precommits_len];
    scale_encoded = &scale_encoded[precommits_len..];

    let num_votes_ancestries: parity_scale_codec::Compact<u64> =
        parity_scale_codec::Decode::decode(&mut scale_encoded)
            .map_err(Error::NumElementsDecodeError)?;
    if num_votes_ancestries.0 != 0 {
        // TODO:
        unimplemented!()
    }
    if scale_encoded.len() != 0 {
        return Err(Error::TooLong);
    }

    Ok(JustificationRef {
        round,
        precommits: PrecommitsRef {
            inner: precommits_data,
        },
    })
}

const PRECOMMIT_ENCODED_LEN: usize = 32 + 4 + 64 + 32;

/// Decoded justification.
// TODO: document and explain
#[derive(Debug)]
pub struct JustificationRef<'a> {
    pub round: u64,
    pub precommits: PrecommitsRef<'a>,
}

/// Decoded justification.
// TODO: document and explain
// TODO: #[derive(Debug)]
pub struct Justification {
    pub round: u64,
    pub precommits: Vec<Precommit>,
}

impl<'a> From<JustificationRef<'a>> for Justification {
    fn from(j: JustificationRef<'a>) -> Justification {
        Justification {
            round: j.round,
            precommits: j.precommits.iter().map(Into::into).collect(),
        }
    }
}

pub struct PrecommitsRef<'a> {
    inner: &'a [u8],
}

impl<'a> PrecommitsRef<'a> {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = PrecommitRef<'a>> + 'a {
        debug_assert_eq!(self.inner.len() % PRECOMMIT_ENCODED_LEN, 0);
        self.inner
            .chunks(PRECOMMIT_ENCODED_LEN)
            .map(|buf| PrecommitRef::from_scale_encoded(buf).unwrap())
    }
}

impl<'a> fmt::Debug for PrecommitsRef<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[derive(Debug)]
pub struct PrecommitRef<'a> {
    /// Hash of the block concerned by the pre-commit.
    pub target_hash: &'a [u8; 32],
    /// Height of the block concerned by the pre-commit.
    pub target_number: u32,

    /// Ed25519 signature made with [`PrecommitRef::authority_public_key`].
    ///
    /// Guaranteed to be 64 bytes. We can't use a `&[u8; 64]` because of limitations in the Rust
    /// type system.
    // TODO: document what is being signed
    // TODO: use &[u8; 64] when possible
    pub signature: &'a [u8],

    /// Authority that signed the precommit. Must be part of the authority set for the
    /// justification to be valid.
    pub authority_public_key: &'a [u8; 32],
}

impl<'a> PrecommitRef<'a> {
    pub fn from_scale_encoded(encoded: &'a [u8]) -> Result<PrecommitRef<'a>, Error> {
        if encoded.len() != PRECOMMIT_ENCODED_LEN {
            return Err(Error::TooShort);
        }

        Ok(PrecommitRef {
            target_hash: TryFrom::try_from(&encoded[0..32]).unwrap(),
            target_number: u32::from_le_bytes(<[u8; 4]>::try_from(&encoded[32..36]).unwrap()),
            signature: &encoded[36..100],
            authority_public_key: TryFrom::try_from(&encoded[100..132]).unwrap(),
        })
    }
}

// TODO: Debug
pub struct Precommit {
    /// Hash of the block concerned by the pre-commit.
    pub target_hash: [u8; 32],
    /// Height of the block concerned by the pre-commit.
    pub target_number: u32,

    /// Ed25519 signature made with [`PrecommitRef::authority_public_key`].
    // TODO: document what is being signed
    // TODO: use &[u8; 64] when possible
    pub signature: [u8; 64],

    /// Authority that signed the precommit. Must be part of the authority set for the
    /// justification to be valid.
    pub authority_public_key: [u8; 32],
}

impl<'a> From<PrecommitRef<'a>> for Precommit {
    fn from(pc: PrecommitRef<'a>) -> Precommit {
        Precommit {
            target_hash: *pc.target_hash,
            target_number: pc.target_number,
            signature: {
                // TODO: ugh
                let mut v = [0; 64];
                v.copy_from_slice(pc.signature);
                v
            },
            authority_public_key: *pc.authority_public_key,
        }
    }
}

/// Potential error when decoding a justification.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Justification is not long enough.
    TooShort,
    /// Justification is too long.
    TooLong,
    NumElementsDecodeError(parity_scale_codec::Error),
}
