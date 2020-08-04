use super::unsealed;
use crate::{babe, executor, header, trie::calculate_root};

use core::time::Duration;
use hashbrown::HashMap;

/// Configuration for a block verification.
// TODO: don't pass functions to the Config; instead, have a state-machine-like API
pub struct Config<'a, TBody> {
    /// Runtime used to check the new block. Must be built using the `:code` of the parent
    /// block.
    pub parent_runtime: executor::WasmVmPrototype,

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

    /// Body of the block to verify.
    pub block_body: TBody,

    /// Optional cache corresponding to the storage trie root hash calculation.
    pub top_trie_root_calculation_cache: Option<calculate_root::CalculationCache>,
}

/// Block successfully verified.
pub struct Success {
    /// Runtime that was passed by [`Config`].
    pub parent_runtime: executor::WasmVmPrototype,

    /// If `Some`, the block is the first block of a new BABE epoch. Returns the information about
    /// the epoch.
    pub babe_epoch_change: Option<babe::EpochChangeInformation>,

    /// Slot number the block belongs to.
    pub slot_number: u64,

    /// List of changes to the storage top trie that the block performs.
    pub storage_top_trie_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,

    /// List of changes to the offchain storage that this block performs.
    pub offchain_storage_changes: HashMap<Vec<u8>, Option<Vec<u8>>, fnv::FnvBuildHasher>,

    /// Cache used for calculating the top trie root.
    pub top_trie_root_calculation_cache: calculate_root::CalculationCache,
    // TOOD: logs written by the runtime
}

/// Error that can happen during the verification.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Error while verifying the unsealed block.
    Unsealed(unsealed::Error),
    /// Failed to verify the authenticity of the block with the BABE algorithm.
    BabeVerification(babe::VerifyError),
}

/// Verifies whether a block is valid.
pub fn verify<'a>(
    config: Config<'a, impl ExactSizeIterator<Item = impl AsRef<[u8]> + Clone> + Clone>,
) -> Verify {
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

    // BABE adds a seal at the end of the digest logs. This seal is guaranteed to be the last
    // item. We need to remove it before we can verify the unsealed header.
    let mut unsealed_header = config.block_header.clone();
    let _seal_log = unsealed_header.digest.pop().unwrap();

    let import_process = {
        let result = unsealed::verify_unsealed_block(unsealed::Config {
            parent_runtime: config.parent_runtime,
            block_header: unsealed_header,
            block_body: config.block_body,
            top_trie_root_calculation_cache: config.top_trie_root_calculation_cache,
        });

        match result {
            Ok(i) => i,
            Err(err) => return Verify::Finished(Err(Error::Unsealed(err))),
        }
    };

    Verify::ReadyToRun(ReadyToRun {
        inner: ReadyToRunInner::Babe {
            babe_verification,
            import_process,
        },
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
    /// Loading a storage value is required in order to continue.
    StorageGet(StorageGet),
    /// Fetching the list of keys with a given prefix is required in order to continue.
    PrefixKeys(PrefixKeys),
    /// Fetching the key that follows a given one is required in order to continue.
    NextKey(NextKey),
}

/// Verification is ready to continue.
#[must_use]
pub struct ReadyToRun {
    inner: ReadyToRunInner,
}

enum ReadyToRunInner {
    /// Verifying BABE.
    Babe {
        babe_verification: babe::SuccessOrPending,
        import_process: unsealed::ReadyToRun,
    },
    /// Error in BABE verification.
    BabeError(babe::VerifyError),
    /// Verifying the unsealed block.
    Unsealed {
        inner: unsealed::ReadyToRun,
        babe_success: babe::VerifySuccess,
    },
}

impl ReadyToRun {
    /// Continues the verification.
    pub fn run(self) -> Verify {
        match self.inner {
            ReadyToRunInner::Babe {
                babe_verification,
                import_process,
            } => match babe_verification {
                babe::SuccessOrPending::Success(babe_success) => Verify::ReadyToRun(ReadyToRun {
                    inner: ReadyToRunInner::Unsealed {
                        inner: import_process,
                        babe_success,
                    },
                }),
                babe::SuccessOrPending::Pending(pending) => {
                    Verify::EpochInformation(EpochInformation {
                        inner: pending,
                        import_process,
                    })
                }
            },
            ReadyToRunInner::BabeError(err) => Verify::Finished(Err(Error::BabeVerification(err))),
            ReadyToRunInner::Unsealed {
                inner,
                babe_success,
            } => match inner.run() {
                unsealed::Verify::Finished(Err(err)) => Verify::Finished(Err(Error::Unsealed(err))),
                unsealed::Verify::Finished(Ok(success)) => Verify::Finished(Ok(Success {
                    parent_runtime: success.parent_runtime,
                    babe_epoch_change: babe_success.epoch_change,
                    slot_number: babe_success.slot_number,
                    storage_top_trie_changes: success.storage_top_trie_changes,
                    offchain_storage_changes: success.offchain_storage_changes,
                    top_trie_root_calculation_cache: success.top_trie_root_calculation_cache,
                })),
                unsealed::Verify::ReadyToRun(inner) => Verify::ReadyToRun(ReadyToRun {
                    inner: ReadyToRunInner::Unsealed {
                        inner,
                        babe_success,
                    },
                }),
                unsealed::Verify::StorageGet(inner) => Verify::StorageGet(StorageGet {
                    inner,
                    babe_success,
                }),
                unsealed::Verify::PrefixKeys(inner) => Verify::PrefixKeys(PrefixKeys {
                    inner,
                    babe_success,
                }),
                unsealed::Verify::NextKey(inner) => Verify::NextKey(NextKey {
                    inner,
                    babe_success,
                }),
            },
        }
    }
}

/// Fetching an epoch information is required in order to continue.
#[must_use]
pub struct EpochInformation {
    inner: babe::PendingVerify,
    import_process: unsealed::ReadyToRun,
}

impl EpochInformation {
    /// Returns the epoch number whose information must be passed to
    /// [`EpochInformation::inject_epoch`].
    pub fn epoch_number(&self) -> u64 {
        self.inner.epoch_number()
    }

    /// Finishes the verification. Must provide the information about the epoch whose number is
    /// obtained with [`EpochInformation::epoch_number`].
    pub fn inject_epoch(self, epoch_info: &babe::EpochInformation) -> ReadyToRun {
        match self.inner.finish(epoch_info) {
            Ok(babe_success) => ReadyToRun {
                inner: ReadyToRunInner::Unsealed {
                    inner: self.import_process,
                    babe_success,
                },
            },
            Err(err) => ReadyToRun {
                inner: ReadyToRunInner::BabeError(err),
            },
        }
    }
}

/// Loading a storage value is required in order to continue.
#[must_use]
pub struct StorageGet {
    inner: unsealed::StorageGet,
    babe_success: babe::VerifySuccess,
}

impl StorageGet {
    /// Returns the key whose value must be passed to [`StorageGet::inject_value`].
    // TODO: shouldn't be mut
    pub fn key<'a>(&'a mut self) -> impl Iterator<Item = impl AsRef<[u8]> + 'a> + 'a {
        self.inner.key()
    }

    /// Injects the corresponding storage value.
    // TODO: change API, see unsealed::StorageGet
    pub fn inject_value(self, value: Option<&[u8]>) -> ReadyToRun {
        ReadyToRun {
            inner: ReadyToRunInner::Unsealed {
                inner: self.inner.inject_value(value),
                babe_success: self.babe_success,
            },
        }
    }
}

/// Fetching the list of keys with a given prefix is required in order to continue.
#[must_use]
pub struct PrefixKeys {
    inner: unsealed::PrefixKeys,
    babe_success: babe::VerifySuccess,
}

impl PrefixKeys {
    /// Returns the prefix whose keys to load.
    // TODO: don't take &mut mut but &self
    pub fn prefix(&mut self) -> &[u8] {
        self.inner.prefix()
    }

    /// Injects the list of keys.
    pub fn inject_keys(self, keys: impl Iterator<Item = impl AsRef<[u8]>>) -> ReadyToRun {
        ReadyToRun {
            inner: ReadyToRunInner::Unsealed {
                inner: self.inner.inject_keys(keys),
                babe_success: self.babe_success,
            },
        }
    }
}

/// Fetching the key that follows a given one is required in order to continue.
#[must_use]
pub struct NextKey {
    inner: unsealed::NextKey,
    babe_success: babe::VerifySuccess,
}

impl NextKey {
    /// Returns the key whose next key must be passed back.
    // TODO: don't take &mut mut but &self
    pub fn key(&mut self) -> &[u8] {
        self.inner.key()
    }

    /// Injects the key.
    pub fn inject_key(self, key: Option<impl AsRef<[u8]>>) -> ReadyToRun {
        ReadyToRun {
            inner: ReadyToRunInner::Unsealed {
                inner: self.inner.inject_key(key),
                babe_success: self.babe_success,
            },
        }
    }
}
