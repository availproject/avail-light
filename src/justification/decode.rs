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
        target_hash,
        target_number,
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
    pub target_hash: &'a [u8; 32],
    pub target_number: u32,
    pub precommits: PrecommitsRef<'a>,
}

/// Decoded justification.
// TODO: document and explain
#[derive(Debug)]
pub struct Justification {
    pub round: u64,
    pub target_hash: [u8; 32],
    pub target_number: u32,
    pub precommits: Vec<Precommit>,
}

impl<'a> From<JustificationRef<'a>> for Justification {
    fn from(j: JustificationRef<'a>) -> Justification {
        Justification {
            round: j.round,
            target_hash: *j.target_hash,
            target_number: j.target_number,
            precommits: j.precommits.iter().map(Into::into).collect(),
        }
    }
}

#[derive(Copy, Clone)]
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

impl fmt::Debug for Precommit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Precommit")
            .field("target_hash", &self.target_hash)
            .field("target_number", &self.target_number)
            .field("signature", &&self.signature[..])
            .field("authority_public_key", &self.authority_public_key)
            .finish()
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

#[cfg(test)]
mod tests {
    #[test]
    fn decode() {
        super::decode(&[
            7, 181, 6, 0, 0, 0, 0, 0, 41, 241, 171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160,
            115, 76, 8, 195, 253, 109, 240, 108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210,
            0, 158, 4, 0, 20, 41, 241, 171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160, 115,
            76, 8, 195, 253, 109, 240, 108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210, 0,
            158, 4, 0, 13, 247, 129, 120, 204, 170, 120, 173, 41, 241, 213, 234, 121, 111, 20, 38,
            193, 94, 99, 139, 57, 30, 71, 209, 236, 222, 165, 123, 70, 139, 71, 65, 36, 142, 39,
            13, 94, 240, 44, 174, 150, 85, 149, 223, 166, 82, 210, 103, 40, 129, 102, 26, 212, 116,
            231, 209, 163, 107, 49, 82, 229, 197, 82, 8, 28, 21, 28, 17, 203, 114, 51, 77, 38, 215,
            7, 105, 227, 175, 123, 191, 243, 128, 26, 78, 45, 202, 43, 9, 183, 204, 224, 175, 141,
            216, 19, 7, 41, 241, 171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160, 115, 76, 8,
            195, 253, 109, 240, 108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210, 0, 158, 4,
            0, 62, 37, 145, 44, 21, 192, 120, 229, 236, 113, 122, 56, 193, 247, 45, 210, 184, 12,
            62, 220, 253, 147, 70, 133, 85, 18, 90, 167, 201, 118, 23, 107, 184, 187, 3, 104, 170,
            132, 17, 18, 89, 77, 156, 145, 242, 8, 185, 88, 74, 87, 21, 52, 247, 101, 57, 154, 163,
            5, 130, 20, 15, 230, 8, 3, 104, 13, 39, 130, 19, 249, 8, 101, 138, 73, 161, 2, 90, 127,
            70, 108, 25, 126, 143, 182, 250, 187, 94, 98, 34, 10, 123, 215, 95, 134, 12, 171, 41,
            241, 171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160, 115, 76, 8, 195, 253, 109,
            240, 108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210, 0, 158, 4, 0, 125, 172, 79,
            71, 1, 38, 137, 128, 232, 95, 70, 104, 217, 95, 7, 58, 28, 114, 182, 216, 171, 56, 231,
            218, 199, 244, 220, 122, 6, 225, 5, 175, 172, 47, 198, 61, 84, 42, 75, 66, 62, 90, 243,
            18, 58, 36, 108, 235, 132, 103, 136, 38, 164, 164, 237, 164, 41, 225, 152, 157, 146,
            237, 24, 11, 142, 89, 54, 135, 0, 234, 137, 226, 191, 137, 34, 204, 158, 75, 134, 214,
            101, 29, 28, 104, 154, 13, 87, 129, 63, 151, 104, 219, 170, 222, 207, 113, 41, 241,
            171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160, 115, 76, 8, 195, 253, 109, 240,
            108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210, 0, 158, 4, 0, 68, 192, 211, 142,
            239, 33, 55, 222, 165, 127, 203, 155, 217, 170, 61, 95, 206, 74, 74, 19, 123, 60, 67,
            142, 80, 18, 175, 40, 136, 156, 151, 224, 191, 157, 91, 187, 39, 185, 249, 212, 158,
            73, 197, 90, 54, 222, 13, 76, 181, 134, 69, 3, 165, 248, 94, 196, 68, 186, 80, 218, 87,
            162, 17, 11, 222, 166, 244, 167, 39, 211, 178, 57, 146, 117, 214, 238, 136, 23, 136,
            31, 16, 89, 116, 113, 220, 29, 39, 241, 68, 41, 90, 214, 251, 147, 60, 122, 41, 241,
            171, 236, 144, 172, 25, 157, 240, 109, 238, 59, 160, 115, 76, 8, 195, 253, 109, 240,
            108, 170, 63, 120, 149, 47, 143, 149, 22, 64, 88, 210, 0, 158, 4, 0, 58, 187, 123, 135,
            2, 157, 81, 197, 40, 200, 218, 52, 253, 193, 119, 104, 190, 246, 221, 225, 175, 195,
            177, 218, 209, 175, 83, 119, 98, 175, 196, 48, 67, 76, 59, 223, 13, 202, 48, 1, 10, 99,
            200, 201, 123, 29, 89, 131, 120, 70, 162, 235, 11, 191, 96, 57, 83, 51, 217, 199, 35,
            50, 174, 2, 247, 45, 175, 46, 86, 14, 79, 15, 34, 251, 92, 187, 4, 173, 29, 127, 238,
            133, 10, 171, 35, 143, 208, 20, 193, 120, 118, 158, 126, 58, 155, 132, 0,
        ])
        .unwrap();
    }
}
