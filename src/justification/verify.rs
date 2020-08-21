use crate::justification::decode;
use core::convert::TryFrom as _;

#[derive(Debug)]
pub struct Config<'a> {
    /// Justification in SCALE encoding.
    pub scale_encoded_justification: &'a [u8],
}

/// Verifies that a justification is valid.
pub fn verify<'a>(config: Config<'a>) -> Result<(), Error> {
    let decoded = decode::decode(&config.scale_encoded_justification).map_err(Error::Decode)?;

    // We verify all the signatures together.
    {
        let mut messages = Vec::with_capacity(decoded.precommits.iter().len());
        let mut signatures = Vec::with_capacity(decoded.precommits.iter().len());
        let mut public_keys = Vec::with_capacity(decoded.precommits.iter().len());

        for precommit in decoded.precommits.iter() {
            messages.push({
                let mut msg = Vec::with_capacity(1 + 32 + 4 + 8 + 8);
                msg.push(1u8); // This `1` indicates which kind of message is being signed.
                msg.extend_from_slice(&precommit.target_hash[..]);
                msg.extend_from_slice(&u32::to_le_bytes(precommit.target_number)[..]);
                msg.extend_from_slice(&u64::to_le_bytes(decoded.round)[..]);
                // This `0` is the `set_id` parameter. It is a dummy 0 value.
                msg.extend_from_slice(&u64::to_le_bytes(0)[..]);
                debug_assert_eq!(msg.len(), msg.capacity());
                msg
            });

            signatures.push(ed25519_dalek::Signature::try_from(precommit.signature).unwrap());

            public_keys.push(
                ed25519_dalek::PublicKey::from_bytes(precommit.authority_public_key)
                    .map_err(|_| Error::BadSignature)?,
            );
        }

        debug_assert_eq!(messages.len(), public_keys.len());
        debug_assert_eq!(messages.len(), signatures.len());
        debug_assert_eq!(public_keys.len(), signatures.len());

        debug_assert_eq!(messages.len(), messages.capacity());
        debug_assert_eq!(signatures.len(), signatures.capacity());
        debug_assert_eq!(public_keys.len(), public_keys.capacity());

        let messages_refs = messages.iter().map(|m| &m[..]).collect::<Vec<_>>();
        ed25519_dalek::verify_batch(&messages_refs, &signatures, &public_keys)
            .map_err(|_| Error::BadSignature)?;
    }

    // TODO:
    Ok(())
}

/// Error that can happen while verifying a justification.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Error while decoding the justification.
    Decode(decode::Error),
    /// One of the signatures can't be verified.
    BadSignature,
}
