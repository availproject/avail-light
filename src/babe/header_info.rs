//! Information extraction from a block header.

use super::definitions;
use crate::header;
use parity_scale_codec::DecodeAll as _;

// TODO: move these definitions locally
pub use definitions::{NextConfigDescriptor, PreDigest};

// TODO: merge this with the `crate::header` module

/// Information successfully extracted from the header.
#[derive(Debug)]
pub struct HeaderInfo<'a> {
    /// Block signature stored in the header.
    ///
    /// Part of the rules is that the last digest log of the header must always be the seal,
    /// containing a signature of the rest of the header and made by the author of the block.
    pub seal_signature: &'a [u8],

    /// Information about the slot claim made by the block author.
    // TODO: use a different type
    pub pre_runtime: PreDigest,

    /// Information about an epoch change, and additionally potentially to the BABE configuration.
    // TODO: replace `NextConfigDescriptor` with different type
    pub epoch_change: Option<(EpochInformation, Option<NextConfigDescriptor>)>,
}

impl<'a> HeaderInfo<'a> {
    /// Returns the slot number stored in the header.
    pub fn slot_number(&self) -> u64 {
        match &self.pre_runtime {
            PreDigest::Primary(digest) => digest.slot_number,
            PreDigest::SecondaryPlain(digest) => digest.slot_number,
            PreDigest::SecondaryVRF(digest) => digest.slot_number,
        }
    }
}

/// Information about an epoch.
///
/// Obtained as part of the [`VerifySuccess`] returned after verifying a block.
#[derive(Debug, Clone)]
pub struct EpochInformation {
    /// List of authorities that are allowed to sign blocks during this epoch.
    ///
    /// The order of the authorities in the list is important, as blocks contain the index, in
    /// that list, of the authority that signed them.
    pub authorities: Vec<EpochInformationAuthority>,

    /// High-entropy data that can be used as a source of randomness during this epoch. Built
    /// by the runtime using the VRF output of all the blocks in the previous epoch.
    pub randomness: [u8; 32],
}

/// Information about a specific authority.
#[derive(Debug, Clone)]
pub struct EpochInformationAuthority {
    /// Ristretto public key that is authorized to sign blocks.
    pub public_key: [u8; 32],

    /// An arbitrary weight value applied to this authority.
    ///
    /// These values don't have any meaning in the absolute, only relative to each other. An
    /// authority with twice the weight value as another authority will be able to claim twice as
    /// many slots.
    pub weight: u64,
}

/// Failure to get the information from the header.
///
/// Indicates that the header is malformed.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// The seal (containing the signature of the authority) is missing from the header.
    MissingSeal,
    /// No pre-runtime digest in the block header.
    MissingPreRuntimeDigest,
    /// There are multiple pre-runtime digests in the block header.
    MultiplePreRuntimeDigests,
    /// Failed to decode pre-runtime digest.
    PreRuntimeDigestDecodeError(parity_scale_codec::Error),
    /// Failed to decode a consensus digest.
    ConsensusDigestDecodeError(parity_scale_codec::Error),
    /// There are multiple epoch descriptor digests in the block header.
    MultipleEpochDescriptors,
    /// There are multiple configuration descriptor digests in the block header.
    MultipleConfigDescriptors,
    /// Found a configuration change digest without an epoch change digest.
    UnexpectedConfigDescriptor,
}

/// Returns the information stored in a certain header.
pub fn header_information(header: header::HeaderRef) -> Result<HeaderInfo, Error> {
    let seal_signature: &[u8] = header
        .digest
        .logs()
        .last()
        .and_then(|l| match l {
            header::DigestItemRef::Seal(engine, signature) if engine == b"BABE" => Some(signature),
            _ => None,
        })
        .ok_or(Error::MissingSeal)?;

    // Additionally, one of the digest logs of the header must be a BABE pre-runtime digest whose
    // content contains the slot claim made by the author.
    let pre_runtime: PreDigest = {
        let mut pre_runtime_digests = header.digest.logs().filter_map(|l| match l {
            header::DigestItemRef::PreRuntime(engine, data) if engine == b"BABE" => Some(data),
            _ => None,
        });
        let pre_runtime = pre_runtime_digests
            .next()
            .ok_or(Error::MissingPreRuntimeDigest)?;
        if pre_runtime_digests.next().is_some() {
            return Err(Error::MultiplePreRuntimeDigests);
        }
        PreDigest::decode_all(&pre_runtime).map_err(Error::PreRuntimeDigestDecodeError)?
    };

    // Finally, the header can contain consensus digest logs, indicating an epoch transition or
    // a configuration change.
    let consensus_logs: Vec<definitions::ConsensusLog> = {
        let list = header.digest.logs().filter_map(|l| match l {
            header::DigestItemRef::Consensus(engine, data) if engine == b"BABE" => Some(data),
            _ => None,
        });

        let mut consensus_logs = Vec::with_capacity(header.digest.logs().len());
        for digest in list {
            let decoded = definitions::ConsensusLog::decode_all(&digest)
                .map_err(Error::ConsensusDigestDecodeError)?;
            consensus_logs.push(decoded);
        }
        consensus_logs
    };

    // Amongst `consensus_logs`, try to find the epoch and config change logs.
    let epoch_change = {
        let mut epoch_change = None;
        let mut config_change = None;

        for consensus_log in consensus_logs {
            match consensus_log {
                definitions::ConsensusLog::NextEpochData(_) if epoch_change.is_some() => {
                    return Err(Error::MultipleEpochDescriptors)
                }
                definitions::ConsensusLog::NextEpochData(data) => epoch_change = Some(data),
                definitions::ConsensusLog::NextConfigData(_) if config_change.is_some() => {
                    return Err(Error::MultipleConfigDescriptors)
                }
                definitions::ConsensusLog::NextConfigData(data) => config_change = Some(data),
                definitions::ConsensusLog::OnDisabled(_) => todo!(), // TODO: unimplemented
            }
        }

        let epoch_change = epoch_change.map(|epoch| EpochInformation {
            randomness: epoch.randomness,
            authorities: epoch
                .authorities
                .into_iter()
                .map(|(public_key, weight)| EpochInformationAuthority { public_key, weight })
                .collect(),
        });

        match (epoch_change, config_change) {
            (None, Some(_)) => return Err(Error::UnexpectedConfigDescriptor),
            (None, None) => None,
            (Some(e), Some(c)) => Some((e, Some(c))),
            (Some(e), None) => Some((e, None)),
        }
    };

    Ok(HeaderInfo {
        seal_signature,
        pre_runtime,
        epoch_change,
    })
}
