//! Information extraction from a block header.

use crate::header;

// TODO: remove this module altogether

/// Information successfully extracted from the header.
#[derive(Debug)]
pub struct HeaderInfo<'a> {
    /// Block signature stored in the header.
    ///
    /// Guaranteed to be 64 bytes long.
    // TODO: we don't use a &[u8; 64] because traits aren't defined on this type; need to fix after Rust gets proper support
    pub seal_signature: &'a [u8],

    /// Information about the slot claim made by the block author.
    pub pre_runtime: header::BabePreDigestRef<'a>,

    /// Information about an epoch change, and additionally potentially to the BABE configuration.
    pub epoch_change: Option<(EpochInformation, Option<ConfigurationChange>)>,
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

/// Change in the configuration at an epoch transition.
#[derive(Debug, Clone, PartialEq, Eq, parity_scale_codec::Encode, parity_scale_codec::Decode)]
pub struct ConfigurationChange {
    /// A constant value that is used in the threshold calculation formula.
    /// Expressed as a rational where the first member of the tuple is the numerator and the
    /// second is the denominator. The rational should represent a value between 0 and 1.
    ///
    /// In the threshold formula calculation, `1 - c` represents the probability of a slot being
    /// empty.
    pub c: (u64, u64),

    /// Whether this chain should run with secondary slot claims.
    pub allowed_slots: header::BabeAllowedSlots,
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
            header::DigestItemRef::BabeSeal(signature) => Some(signature),
            _ => None,
        })
        .ok_or(Error::MissingSeal)?;

    // Additionally, one of the digest logs of the header must be a BABE pre-runtime digest whose
    // content contains the slot claim made by the author.
    let pre_runtime: header::BabePreDigestRef = {
        let mut pre_runtime_digests = header.digest.logs().filter_map(|l| match l {
            header::DigestItemRef::BabePreDigest(item) => Some(item),
            _ => None,
        });
        let pre_runtime = pre_runtime_digests
            .next()
            .ok_or(Error::MissingPreRuntimeDigest)?;
        if pre_runtime_digests.next().is_some() {
            return Err(Error::MultiplePreRuntimeDigests);
        }
        pre_runtime
    };

    // Amongst BABE consensus logs, try to find the epoch and config change logs.
    let epoch_change = {
        let mut epoch_change = None;
        let mut config_change = None;

        let consensus_logs = header.digest.logs().filter_map(|l| match l {
            header::DigestItemRef::BabeConsensus(item) => Some(item),
            _ => None,
        });

        for consensus_log in consensus_logs {
            match consensus_log {
                header::BabeConsensusLogRef::NextEpochData(_) if epoch_change.is_some() => {
                    return Err(Error::MultipleEpochDescriptors)
                }
                header::BabeConsensusLogRef::NextEpochData(data) => epoch_change = Some(data),
                header::BabeConsensusLogRef::NextConfigData(_) if config_change.is_some() => {
                    return Err(Error::MultipleConfigDescriptors)
                }
                header::BabeConsensusLogRef::NextConfigData(data) => config_change = Some(data),
                header::BabeConsensusLogRef::OnDisabled(_) => {} // TODO: unimplemented
            }
        }

        let epoch_change = epoch_change.map(|epoch| EpochInformation {
            randomness: *epoch.randomness,
            authorities: epoch
                .authorities
                .into_iter()
                .map(|auth| EpochInformationAuthority {
                    public_key: *auth.public_key,
                    weight: auth.weight,
                })
                .collect(),
        });

        let config_change = config_change.map(|cfg| ConfigurationChange {
            c: cfg.c,
            allowed_slots: cfg.allowed_slots,
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
