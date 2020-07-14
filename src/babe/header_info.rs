//! Information extraction from a block header.

/// Information successfully extracted from the header.
pub struct HeaderInfo<'a> {
    /// Block signature stored in the header.
    ///
    /// Part of the rules is that the last digest log of the header must always be the seal,
    /// containing a signature of the rest of the header and made by the author of the block.
    pub seal_signature: &'a [u8],
}

/// Failure to get the information from the header.
///
/// Indicates that the header is malformed.
#[derive(Debug, Clone, derive_more::Display)]
pub enum Error {
    /// Header passed is of the wrong format.
    InvalidHeader,
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
pub fn header_information(scale_encoded_header: &[u8]) -> Result<HeaderInfo, VerifyError> {
    let header = crate::block::Header::decode_all(config.scale_encoded_header)
        .map_err(|_| VerifyError::InvalidHeader)?;

    let seal_signature: &Vec<u8> = header
        .digest
        .logs
        .last()
        .and_then(|l| match l {
            crate::block::DigestItem::Seal(engine, signature) if engine == b"BABE" => {
                Some(signature)
            }
            _ => None,
        })
        .ok_or(VerifyError::MissingSeal)?;

    // Additionally, one of the digest logs of the header must be a BABE pre-runtime digest whose
    // content contains the slot claim made by the author.
    let pre_runtime: definitions::PreDigest = {
        let mut pre_runtime_digests = header.digest.logs.iter().filter_map(|l| match l {
            crate::block::DigestItem::PreRuntime(engine, data) if engine == b"BABE" => Some(data),
            _ => None,
        });
        let pre_runtime = pre_runtime_digests
            .next()
            .ok_or(VerifyError::MissingPreRuntimeDigest)?;
        if pre_runtime_digests.next().is_some() {
            return Err(VerifyError::MultiplePreRuntimeDigests);
        }
        definitions::PreDigest::decode_all(&pre_runtime)
            .map_err(VerifyError::PreRuntimeDigestDecodeError)?
    };

    // Finally, the header can contain consensus digest logs, indicating an epoch transition or
    // a configuration change.
    let consensus_logs: Vec<definitions::ConsensusLog> = {
        let list = header.digest.logs.iter().filter_map(|l| match l {
            crate::block::DigestItem::Consensus(engine, data) if engine == b"BABE" => Some(data),
            _ => None,
        });

        let mut consensus_logs = Vec::with_capacity(header.digest.logs.len());
        for digest in list {
            let decoded = definitions::ConsensusLog::decode_all(&digest)
                .map_err(VerifyError::ConsensusDigestDecodeError)?;
            consensus_logs.push(decoded)
        }
        consensus_logs
    };

    // The signature of the block header applies to the header from where the signature isn't
    // present.
    let pre_seal_hash = {
        let mut unsealed_header = header.clone();
        let _popped = unsealed_header.digest.logs.pop();
        debug_assert!(matches!(
            _popped,
            Some(crate::block::DigestItem::Seal(_, _))
        ));
        unsealed_header.block_hash()
    };

    Ok(HeaderInfo {
        seal_signature,
    })
}
