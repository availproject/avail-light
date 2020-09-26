// Copyright (C) 2019-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::{header, verify::babe};

use core::{num::NonZeroU64, time::Duration};

/// Configuration for a block verification.
pub struct Config<'a> {
    /// Header of the parent of the block to verify.
    ///
    /// The hash of this header must be the one referenced in [`Config::block_header`].
    pub parent_block_header: header::HeaderRef<'a>,

    /// BABE configuration retrieved from the genesis block.
    ///
    /// See the documentation of [`babe::BabeGenesisConfiguration`] to know how to get this.
    pub babe_genesis_configuration: &'a babe::BabeGenesisConfiguration,

    /// Slot number of block #1. **Must** be provided, unless the block being verified is block
    /// #1 itself.
    ///
    /// Must be the value of [`Success::slot_number`] for block #1.
    pub block1_slot_number: Option<u64>,

    /// Time elapsed since [the Unix Epoch](https://en.wikipedia.org/wiki/Unix_time) (i.e.
    /// 00:00:00 UTC on 1 January 1970), ignoring leap seconds.
    pub now_from_unix_epoch: Duration,

    /// Header of the block to verify.
    ///
    /// The `parent_hash` field is the hash of the parent whose storage can be accessed through
    /// the other fields.
    pub block_header: header::HeaderRef<'a>,
}

/// Block successfully verified.
pub struct Success {
    /// If `Some`, the verified block contains an epoch transition describing the given epoch.
    /// This epoch transition must later be provided back as part of the [`Config`] when verifying
    /// the blocks that are part of that epoch.
    pub babe_epoch_transition_target: Option<NonZeroU64>,

    /// Slot number the block belongs to.
    pub slot_number: u64,

    /// Epoch number the block belongs to.
    pub epoch_number: u64,
}

/// Error that can happen during the verification.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Number of the block to verify isn't equal to the parent block's number plus one.
    BadBlockNumber,
    /// Hash of the parent block doesn't match the hash in the header to verify.
    BadParentHash,
    /// Failed to verify the authenticity of the block with the BABE algorithm.
    #[display(fmt = "{}", _0)]
    BabeVerification(babe::VerifyError),
}

/// Verifies whether a block is valid.
pub fn verify<'a>(config: Config<'a>) -> Verify {
    // Check that there is no mismatch in the parent header hash.
    // Note that the user is expected to pass a parent block that matches the parent indicated by
    // the header to verify, and not blindly pass an "expected parent". As such, this check is
    // unnecessary and introduces an overhead.
    // However this check is performed anyway, as the consequences of a failure here could be
    // potentially quite high.
    if config.parent_block_header.hash() != *config.block_header.parent_hash {
        return Verify::Finished(Err(Error::BadParentHash));
    }

    // Some basic verification of the block number. This is normally verified by the runtime, but
    // no runtime call can be performed with only the header.
    if config
        .parent_block_header
        .number
        .checked_add(1)
        .map_or(true, |v| v != config.block_header.number)
    {
        return Verify::Finished(Err(Error::BadBlockNumber));
    }

    // Start the BABE verification process.
    let babe_verification = {
        let result = babe::start_verify_header(babe::VerifyConfig {
            header: config.block_header.clone(),
            parent_block_header: config.parent_block_header,
            genesis_configuration: config.babe_genesis_configuration,
            now_from_unix_epoch: config.now_from_unix_epoch,
            block1_slot_number: config.block1_slot_number,
        });

        match result {
            Ok(s) => s,
            Err(err) => return Verify::Finished(Err(Error::BabeVerification(err))),
        }
    };

    // TODO: need to verify the changes trie stuff maybe?
    // TODO: need to verify that there's no grandpa scheduled change header if there's already an active grandpa scheduled change

    Verify::ReadyToRun(ReadyToRun {
        inner: ReadyToRunInner::Babe(babe_verification),
    })
}

/// Current state of the verification.
#[must_use]
pub enum Verify {
    /// Verification is over.
    Finished(Result<Success, Error>),
    /// Verification is ready to continue.
    ReadyToRun(ReadyToRun),
    /// Fetching an epoch information is required in order to continue.
    BabeEpochInformation(BabeEpochInformation),
}

/// Verification is ready to continue.
#[must_use]
pub struct ReadyToRun {
    inner: ReadyToRunInner,
}

enum ReadyToRunInner {
    /// Verification finished
    Finished(Result<babe::VerifySuccess, babe::VerifyError>),
    /// Verifying BABE.
    Babe(babe::SuccessOrPending),
}

impl ReadyToRun {
    /// Continues the verification.
    pub fn run(self) -> Verify {
        match self.inner {
            ReadyToRunInner::Babe(babe_verification) => match babe_verification {
                babe::SuccessOrPending::Success(babe_success) => Verify::ReadyToRun(ReadyToRun {
                    inner: ReadyToRunInner::Finished(Ok(babe_success)),
                }),
                babe::SuccessOrPending::Pending(pending) => {
                    Verify::BabeEpochInformation(BabeEpochInformation { inner: pending })
                }
            },
            ReadyToRunInner::Finished(Ok(s)) => Verify::Finished(Ok(Success {
                babe_epoch_transition_target: s.epoch_transition_target,
                slot_number: s.slot_number,
                epoch_number: s.epoch_number,
            })),
            ReadyToRunInner::Finished(Err(err)) => {
                Verify::Finished(Err(Error::BabeVerification(err)))
            }
        }
    }
}

/// Fetching an epoch information is required in order to continue.
#[must_use]
pub struct BabeEpochInformation {
    inner: babe::PendingVerify,
}

impl BabeEpochInformation {
    /// Returns the epoch number whose information must be passed to
    /// [`BabeEpochInformation::inject_epoch`].
    pub fn epoch_number(&self) -> u64 {
        self.inner.epoch_number()
    }

    /// Returns true if the epoch of the verified block is the same as its parent's.
    pub fn same_epoch_as_parent(&self) -> bool {
        self.inner.same_epoch_as_parent()
    }

    /// Finishes the verification. Must provide the information about the epoch whose number is
    /// obtained with [`BabeEpochInformation::epoch_number`].
    pub fn inject_epoch(
        self,
        epoch_info: (header::BabeNextEpochRef, header::BabeNextConfig),
    ) -> ReadyToRun {
        match self.inner.finish(epoch_info) {
            Ok(success) => ReadyToRun {
                inner: ReadyToRunInner::Finished(Ok(success)),
            },
            Err(err) => ReadyToRun {
                inner: ReadyToRunInner::Finished(Err(err)),
            },
        }
    }
}
