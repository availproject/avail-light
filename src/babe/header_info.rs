//! Information extraction from a block header.

use super::definitions;
use crate::header;
use parity_scale_codec::DecodeAll as _;

// TODO: move these definitions locally
pub use definitions::{NextConfigDescriptor, NextEpochDescriptor, PreDigest};

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
    // TODO: use different types
    pub epoch_change: Option<(NextEpochDescriptor, Option<NextConfigDescriptor>)>,
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
