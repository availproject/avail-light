use crate::{header, verify::babe};

use core::time::Duration;

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
    /// If `Some`, the block is the first block of a new BABE epoch. Returns the information about
    /// the epoch.
    pub babe_epoch_change: Option<babe::EpochChangeInformation>,

    /// Slot number the block belongs to.
    pub slot_number: u64,
}

/// Error that can happen during the verification.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Failed to verify the authenticity of the block with the BABE algorithm.
    BabeVerification(babe::VerifyError),
}

/// Verifies whether a block is valid.
pub fn verify<'a>(config: Config<'a>) -> Verify {
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
    EpochInformation(EpochInformation),
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
                    Verify::EpochInformation(EpochInformation { inner: pending })
                }
            },
            ReadyToRunInner::Finished(Ok(s)) => Verify::Finished(Ok(Success {
                babe_epoch_change: s.epoch_change,
                slot_number: s.slot_number,
            })),
            ReadyToRunInner::Finished(Err(err)) => {
                Verify::Finished(Err(Error::BabeVerification(err)))
            }
        }
    }
}

/// Fetching an epoch information is required in order to continue.
#[must_use]
pub struct EpochInformation {
    inner: babe::PendingVerify,
}

impl EpochInformation {
    /// Returns the epoch number whose information must be passed to
    /// [`EpochInformation::inject_epoch`].
    pub fn epoch_number(&self) -> u64 {
        self.inner.epoch_number()
    }

    /// Finishes the verification. Must provide the information about the epoch whose number is
    /// obtained with [`EpochInformation::epoch_number`].
    pub fn inject_epoch(self, epoch_info: header::BabeNextEpochRef) -> ReadyToRun {
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
