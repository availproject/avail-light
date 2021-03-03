// Smoldot
// Copyright (C) 2019-2021  Parity Technologies (UK) Ltd.
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

//! Parachains and parathreads syncing.
//!
//! A parachain is a blockchain whose best and finalized blocks are determined by looking at the
//! storage of a different chain called the relay chain.
//!
//! In order to obtain the current best block of a parachain, you must obtain the parachains
//! persisted validation data from the relay chain. This is done by calling the
//! `ParachainHost_persisted_validation_data` runtime function. The runtime function returns
//! an opaque set of bytes called the "head data" whose meaning depends on the parachain. Most of
//! the time, it is a block hash.
//!
//! In order to obtain the current finalized block of a parachain, do the same but on the current
//! finalized block of the relay chain.
//!
//! See the [`persisted_validation_data_parameters`] to obtain the input to pass to the runtime
//! function. The first parameter is a `para_id` found in the chain specification of the
//! parachain of parathread.

// TODO: at the time of writing of this comment, parachains aren't shipped yet, and everything might still change
// see https://github.com/paritytech/polkadot/blob/master/primitives/src/v1.rs for the reference version

use core::{convert::TryFrom as _, iter};

/// Produces the input to pass to the `ParachainHost_persisted_validation_data` runtime call.
pub fn persisted_validation_data_parameters(
    para_id: u32,
    assumption: OccupiedCoreAssumption,
) -> impl Iterator<Item = impl AsRef<[u8]>> + Clone {
    iter::once(either::Left(para_id.to_le_bytes()))
        .chain(iter::once(either::Right(assumption.scale_encoded())))
}

/// An assumption being made about the state of an occupied core.
// TODO: what does that mean?
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum OccupiedCoreAssumption {
    /// The candidate occupying the core was made available and included to free the core.
    Included,
    /// The candidate occupying the core timed out and freed the core without advancing the para.
    TimedOut,
    /// The core was not occupied to begin with.
    Free,
}

impl OccupiedCoreAssumption {
    /// Returns the SCALE encoding of this type.
    pub fn scale_encoded(&self) -> impl AsRef<[u8]> + Clone {
        match self {
            OccupiedCoreAssumption::Included => [0],
            OccupiedCoreAssumption::TimedOut => [1],
            OccupiedCoreAssumption::Free => [2],
        }
    }
}

/// Attempt to decode the return value of the  `ParachainHost_persisted_validation_data` runtime
/// call.
pub fn decode_persisted_validation_data_return_value(
    scale_encoded: &[u8],
) -> Result<Option<PersistedValidationDataRef>, Error> {
    match nom::combinator::all_consuming(crate::util::nom_option_decode(persisted_validation_data))(
        scale_encoded,
    ) {
        Ok((_, data)) => Ok(data),
        Err(err) => Err(Error(err)),
    }
}

/// Error that can happen during the decoding.
#[derive(Debug, derive_more::Display)]
pub struct Error<'a>(nom::Err<nom::error::Error<&'a [u8]>>);

/// Decoded persisted validation data.
// TODO: document and explain
#[derive(Debug)]
pub struct PersistedValidationDataRef<'a> {
    /// Opaque data representing the best block (or similar concept) of the parachain/parathread.
    ///
    /// The meaning of this data depends on the chain, but most of the time it consists in a
    /// block hash.
    pub parent_head: &'a [u8],
    pub relay_parent_number: u32,
    pub relay_parent_storage_root: &'a [u8; 32],
    pub max_pov_size: u32,
}

/// Nom combinator that parses a [`PersistedValidationDataRef`].
fn persisted_validation_data<'a, E: nom::error::ParseError<&'a [u8]>>(
    bytes: &'a [u8],
) -> nom::IResult<&'a [u8], PersistedValidationDataRef, E> {
    nom::combinator::map(
        nom::sequence::tuple((
            nom::multi::length_data(crate::util::nom_scale_compact_usize),
            nom::number::complete::le_u32,
            nom::bytes::complete::take(32u32),
            nom::number::complete::le_u32,
        )),
        |(parent_head, relay_parent_number, relay_parent_storage_root, max_pov_size)| {
            PersistedValidationDataRef {
                parent_head,
                relay_parent_number,
                relay_parent_storage_root: <&[u8; 32]>::try_from(relay_parent_storage_root)
                    .unwrap(),
                max_pov_size,
            }
        },
    )(bytes)
}

// TODO: add tests
