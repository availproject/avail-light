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

use crate::header;
use core::{convert::TryFrom as _, num::NonZeroU64, time::Duration};

/// Configuration for [`next_slot_claim`].
pub struct Config<'a, TLocAuth> {
    /// Time elapsed since [the Unix Epoch](https://en.wikipedia.org/wiki/Unix_time) (i.e.
    /// 00:00:00 UTC on 1 January 1970), ignoring leap seconds.
    pub now_from_unix_epoch: Duration,

    /// Duration, in milliseconds, of an Aura slot.
    pub slot_duration: NonZeroU64,

    /// List of the Aura authorities allowed to produce a block. This is either the same as the
    /// ones of the current best block, or a new list if the current best block contains an
    /// authorities list change digest item.
    pub current_authorities: header::AuraAuthoritiesIter<'a>,

    /// Iterator to the list of sr25519 public keys available locally.
    ///
    /// Must implement `Iterator<Item = &[u8; 32]>`.
    pub local_authorities: TLocAuth,
}

/// Calculates the earliest one of the authorities in [`Config::local_authorities`] is allowed to
/// produce a block.
///
/// Returns `None` if none of the local authorities are allowed to produce blocks.
///
/// The value returned by this function is entirely deterministic based on the [`Config`] and
/// never changes until [`Config::now_from_unix_epoch`] gets past the value returned in
/// [`SlotClaim::slot_end_from_unix_epoch`].
///
/// However, keep in mind that, as the best block changes, the list of authorities
/// ([`Config::current_authorities`]) might change, in which case this function should be
/// called again.
pub fn next_slot_claim<'a>(
    config: Config<'a, impl Iterator<Item = &'a [u8; 32]>>,
) -> Option<SlotClaim> {
    let num_current_authorities = config.current_authorities.clone().count();

    let current_slot = config.now_from_unix_epoch.as_secs() / config.slot_duration.get();

    let current_slot_index =
        usize::try_from(current_slot.checked_div(u64::try_from(num_current_authorities).unwrap())?)
            .unwrap();

    let mut claim = None;

    for (pub_key_index, local_pub_key) in config.local_authorities.enumerate() {
        // TODO: O(n) complexity
        let mut index = match config
            .current_authorities
            .clone()
            .position(|pk| pk.public_key == local_pub_key)
        {
            Some(idx) => idx,
            None => continue,
        };

        if index < current_slot_index {
            index += num_current_authorities;
        }

        let claimable_slot = current_slot + u64::try_from(index - current_slot_index).unwrap();

        match claim {
            Some((s, _)) if s <= claimable_slot => {}
            _ => claim = Some((claimable_slot, pub_key_index)),
        }
    }

    if let Some((slot_number, local_authorities_index)) = claim {
        let slot_start_from_unix_epoch =
            Duration::from_secs(slot_number * config.slot_duration.get());
        let slot_end_from_unix_epoch =
            slot_start_from_unix_epoch + Duration::from_secs(config.slot_duration.get());
        debug_assert!(slot_end_from_unix_epoch < config.now_from_unix_epoch);

        Some(SlotClaim {
            slot_start_from_unix_epoch,
            slot_end_from_unix_epoch,
            slot_number,
            local_authorities_index,
        })
    } else {
        None
    }
}

/// Slot happening now or in the future and that can be attributed to one of the authorities in
/// [`Config::local_authorities`].
///
/// See also [`next_slot_claim`].
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SlotClaim {
    /// UNIX time when the slot starts. Can be inferior to the value passed to
    /// [`Config::now_from_unix_epoch`] if the slot has already started.
    pub slot_start_from_unix_epoch: Duration,
    /// UNIX time when the slot ends. Always inferior to the value passed to
    /// [`Config::now_from_unix_epoch`].
    pub slot_end_from_unix_epoch: Duration,
    /// Slot number of the claim. Used when building the block.
    pub slot_number: u64,
    /// Index within [`Config::local_authorities`] of the authority that can produce the block.
    pub local_authorities_index: usize,
}
