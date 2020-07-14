//! Information extraction from a block header.

use super::definitions;
use crate::header;
use parity_scale_codec::DecodeAll as _;

/// Information successfully extracted from the header.
#[derive(Debug)]
pub struct HeaderInfo<'a> {
    /// Block signature stored in the header.
    ///
    /// Part of the rules is that the last digest log of the header must always be the seal,
    /// containing a signature of the rest of the header and made by the author of the block.
    pub seal_signature: &'a [u8],

    /// Slot claim made by the block author.
    // TODO: use a different type
    pub pre_runtime: definitions::PreDigest,

    // Finally, the header can contain consensus digest logs, indicating an epoch transition or
    // a configuration change.
    // TODO: some zero-cost iterator instead
    // TODO: use a different type
    pub consensus_logs: Vec<definitions::ConsensusLog>,
}

/// Failure to get the information from the header.
///
/// Indicates that the header is malformed.
#[derive(Debug, derive_more::Display)]
pub enum Error {
    /// Header passed is of the wrong format.
    InvalidHeader(header::Error),
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
}

/// Returns the information stored in a certain header.
pub fn header_information(scale_encoded_header: &[u8]) -> Result<HeaderInfo, Error> {
    let header = header::decode(scale_encoded_header).map_err(Error::InvalidHeader)?;

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
    let pre_runtime: definitions::PreDigest = {
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
        definitions::PreDigest::decode_all(&pre_runtime)
            .map_err(Error::PreRuntimeDigestDecodeError)?
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

    Ok(HeaderInfo {
        seal_signature,
        pre_runtime,
        consensus_logs,
    })
}
