// Substrate-lite
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

use crate::{
    chain::chain_information,
    header,
    verify::{aura, babe},
};

use alloc::vec::Vec;
use core::{num::NonZeroU64, time::Duration};

/// Configuration for a block verification.
pub struct Config<'a> {
    /// Header of the parent of the block to verify.
    ///
    /// The hash of this header must be the one referenced in [`Config::block_header`].
    pub parent_block_header: header::HeaderRef<'a>,

    /// Header of the block to verify.
    ///
    /// The `parent_hash` field is the hash of the parent whose storage can be accessed through
    /// the other fields.
    pub block_header: header::HeaderRef<'a>,

    /// Configuration items related to the consensus engine.
    pub consensus: ConfigConsensus<'a>,
}

/// Extra items of [`Config`] that are dependant on the consensus engine of the chain.
pub enum ConfigConsensus<'a> {
    /// Any node on the chain is allowed to produce blocks.
    ///
    /// No seal must be present in the header.  // TODO: is this true?
    ///
    /// > **Note**: Be warned that this variant makes it possible for a huge number of blocks to
    /// >           be produced. If this variant is used, the user is encouraged to limit, through
    /// >           other means, the number of blocks being accepted.
    AllAuthorized,

    /// Chain is using the Aura consensus engine.
    Aura {
        /// Aura authorities that must validate the block.
        ///
        /// This list is either equal to the parent's list, or, if the parent changes the list of
        /// authorities, equal to that new modified list.
        // TODO: consider not using a Vec
        current_authorities: Vec<header::AuraAuthorityRef<'a>>,

        /// Duration of a slot in milliseconds.
        /// Can be found by calling the `AuraApi_slot_duration` runtime function.
        slot_duration: NonZeroU64,

        /// Time elapsed since [the Unix Epoch](https://en.wikipedia.org/wiki/Unix_time) (i.e.
        /// 00:00:00 UTC on 1 January 1970), ignoring leap seconds.
        now_from_unix_epoch: Duration,
    },

    /// Chain is using the Babe consensus engine.
    Babe {
        /// Number of slots per epoch in the Babe configuration.
        slots_per_epoch: NonZeroU64,

        /// Epoch the parent block belongs to. Must be `None` if and only if the parent block's
        /// number is 0, as block #0 doesn't belong to any epoch.
        parent_block_epoch: Option<chain_information::BabeEpochInformationRef<'a>>,

        /// Epoch that follows the epoch the parent block belongs to.
        parent_block_next_epoch: chain_information::BabeEpochInformationRef<'a>,

        /// Time elapsed since [the Unix Epoch](https://en.wikipedia.org/wiki/Unix_time) (i.e.
        /// 00:00:00 UTC on 1 January 1970), ignoring leap seconds.
        now_from_unix_epoch: Duration,
    },
}

/// Block successfully verified.
pub enum Success {
    /// [`ConfigConsensus::AllAuthorized`] was passed to [`Config`].
    AllAuthorized,

    /// Chain is using the Aura consensus engine.
    Aura {
        /// True if the list of authorities is modified by this block.
        authorities_change: bool,
    },

    /// Chain is using the Babe consensus engine.
    Babe {
        /// Slot number the block belongs to.
        ///
        /// > **Note**: This is a simple reminder. The value can also be found in the header of the
        /// >           block.
        slot_number: u64,

        /// If `Some`, the verified block contains an epoch transition describing the new
        /// "next epoch". When verifying blocks that are children of this one, the value in this
        /// field must be provided as [`ConfigConsensus::Babe::parent_block_next_epoch`], and the
        /// value previously in [`ConfigConsensus::Babe::parent_block_next_epoch`] must instead be
        /// passed as [`ConfigConsensus::Babe::parent_block_epoch`].
        epoch_transition_target: Option<chain_information::BabeEpochInformation>,
    },
}

/// Error that can happen during the verification.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Number of the block to verify isn't equal to the parent block's number plus one.
    BadBlockNumber,
    /// Hash of the parent block doesn't match the hash in the header to verify.
    BadParentHash,
    /// Block header contains items relevant to multiple consensus engines at the same time.
    MultipleConsensusEngines,
    /// Failed to verify the authenticity of the block with the AURA algorithm.
    #[display(fmt = "{}", _0)]
    AuraVerification(aura::VerifyError),
    /// Failed to verify the authenticity of the block with the BABE algorithm.
    #[display(fmt = "{}", _0)]
    BabeVerification(babe::VerifyError),
}

/// Verifies whether a block is valid.
pub fn verify(config: Config) -> Result<Success, Error> {
    // Check that there is no mismatch in the parent header hash.
    // Note that the user is expected to pass a parent block that matches the parent indicated by
    // the header to verify, and not blindly pass an "expected parent". As such, this check is
    // unnecessary and introduces an overhead.
    // However this check is performed anyway, as the consequences of a failure here could be
    // potentially quite high.
    if config.parent_block_header.hash() != *config.block_header.parent_hash {
        return Err(Error::BadParentHash);
    }

    // Some basic verification of the block number. This is normally verified by the runtime, but
    // no runtime call can be performed with only the header.
    if config
        .parent_block_header
        .number
        .checked_add(1)
        .map_or(true, |v| v != config.block_header.number)
    {
        return Err(Error::BadBlockNumber);
    }

    // TODO: need to verify the changes trie stuff maybe?
    // TODO: need to verify that there's no grandpa scheduled change header if there's already an active grandpa scheduled change
    // TODO: verify that there's no grandpa header items if the chain doesn't use grandpa

    match config.consensus {
        ConfigConsensus::AllAuthorized => {
            if config.block_header.digest.has_any_aura()
                || config.block_header.digest.has_any_babe()
            {
                return Err(Error::MultipleConsensusEngines);
            }

            Ok(Success::AllAuthorized)
        }
        ConfigConsensus::Aura {
            current_authorities,
            slot_duration,
            now_from_unix_epoch,
        } => {
            if config.block_header.digest.has_any_babe() {
                return Err(Error::MultipleConsensusEngines);
            }

            let result = aura::verify_header(aura::VerifyConfig {
                header: config.block_header.clone(),
                parent_block_header: config.parent_block_header,
                now_from_unix_epoch,
                current_authorities: current_authorities.into_iter(),
                slot_duration,
            });

            match result {
                Ok(s) => Ok(Success::Aura {
                    authorities_change: s.authorities_change,
                }),
                Err(err) => Err(Error::AuraVerification(err)),
            }
        }
        ConfigConsensus::Babe {
            parent_block_epoch,
            parent_block_next_epoch,
            slots_per_epoch,
            now_from_unix_epoch,
        } => {
            if config.block_header.digest.has_any_aura() {
                return Err(Error::MultipleConsensusEngines);
            }

            let result = babe::verify_header(babe::VerifyConfig {
                header: config.block_header.clone(),
                parent_block_header: config.parent_block_header,
                parent_block_epoch,
                parent_block_next_epoch,
                slots_per_epoch,
                now_from_unix_epoch,
            });

            match result {
                Ok(s) => Ok(Success::Babe {
                    epoch_transition_target: s.epoch_transition_target,
                    slot_number: s.slot_number,
                }),
                Err(err) => Err(Error::BabeVerification(err)),
            }
        }
    }
}
