// Substrate-lite
// Copyright (C) 2019-2020  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use crate::finality::justification::decode;
use core::convert::TryFrom as _;

/// Configuration for a justification verification process.
#[derive(Debug)]
pub struct Config<'a, I> {
    /// Justification to verify.
    pub justification: decode::JustificationRef<'a>,

    // TODO: document
    pub authorities_set_id: u64,

    /// List of authorities that are allowed to emit pre-commits for the block referred to by
    /// the justification. Must implement `Iterator<Item = impl AsRef<[u8]>> + Clone`, where
    /// each item is the public key of an authority.
    pub authorities_list: I,
}

// TODO: rewrite as a generator-style process?

/// Verifies that a justification is valid.
pub fn verify<'a>(
    config: Config<'a, impl Iterator<Item = impl AsRef<[u8]>> + Clone>,
) -> Result<(), Error> {
    // Verifying all the signatures together brings better performances than verifying them one
    // by one.
    let mut messages = Vec::with_capacity(config.justification.precommits.iter().len());
    let mut signatures = Vec::with_capacity(config.justification.precommits.iter().len());
    let mut public_keys = Vec::with_capacity(config.justification.precommits.iter().len());

    for precommit in config.justification.precommits.iter() {
        if !config
            .authorities_list
            .clone()
            .any(|a| a.as_ref() == precommit.authority_public_key)
        {
            return Err(Error::NotAuthority(*precommit.authority_public_key));
        }

        // TODO: must check signed block ancestry using `votes_ancestries`

        messages.push({
            let mut msg = Vec::with_capacity(1 + 32 + 4 + 8 + 8);
            msg.push(1u8); // This `1` indicates which kind of message is being signed.
            msg.extend_from_slice(&precommit.target_hash[..]);
            msg.extend_from_slice(&u32::to_le_bytes(precommit.target_number)[..]);
            msg.extend_from_slice(&u64::to_le_bytes(config.justification.round)[..]);
            msg.extend_from_slice(&u64::to_le_bytes(config.authorities_set_id)[..]);
            debug_assert_eq!(msg.len(), msg.capacity());
            msg
        });

        // Can only panic in case of bad signature length, which we know can't happen.
        signatures.push(ed25519_dalek::Signature::try_from(&precommit.signature[..]).unwrap());

        public_keys.push(
            ed25519_dalek::PublicKey::from_bytes(precommit.authority_public_key)
                .map_err(|_| Error::BadPublicKey)?,
        );
    }

    debug_assert_eq!(messages.len(), public_keys.len());
    debug_assert_eq!(messages.len(), signatures.len());
    debug_assert_eq!(public_keys.len(), signatures.len());

    debug_assert_eq!(messages.len(), messages.capacity());
    debug_assert_eq!(signatures.len(), signatures.capacity());
    debug_assert_eq!(public_keys.len(), public_keys.capacity());

    {
        let messages_refs = messages.iter().map(|m| &m[..]).collect::<Vec<_>>();
        ed25519_dalek::verify_batch(&messages_refs, &signatures, &public_keys)
            .map_err(|_| Error::BadSignature)?;
    }

    // TODO: must check that votes_ancestries doesn't contain any unused entry
    // TODO: there's also a "ghost" thing?

    Ok(())
}

/// Error that can happen while verifying a justification.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// One of the public keys is invalid.
    BadPublicKey,
    /// One of the signatures can't be verified.
    BadSignature,
    /// One of the public keys isn't in the list of authorities.
    #[display(fmt = "One of the public keys isn't in the list of authorities")]
    NotAuthority([u8; 32]),
}
