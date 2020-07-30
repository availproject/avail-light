//! Definitions copy-pasted from Substrate

// TODO: remove this module altogether

use crate::header::BabeAllowedSlots;

use core::fmt;
use parity_scale_codec::{Decode, Encode};

/// Configuration data used by the BABE consensus engine.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct BabeGenesisConfiguration {
    /// The slot duration in milliseconds for BABE. Currently, only
    /// the value provided by this type at genesis will be used.
    ///
    /// Dynamic slot duration may be supported in the future.
    pub slot_duration: u64,

    /// The duration of epochs in slots.
    pub epoch_length: u64,

    /// A constant value that is used in the threshold calculation formula.
    /// Expressed as a rational where the first member of the tuple is the
    /// numerator and the second is the denominator. The rational should
    /// represent a value between 0 and 1.
    /// In the threshold formula calculation, `1 - c` represents the probability
    /// of a slot being empty.
    pub c: (u64, u64),

    /// The authorities for the genesis epoch.
    pub genesis_authorities: Vec<([u8; 32], u64)>,

    /// The randomness for the genesis epoch.
    pub randomness: [u8; 32],

    /// Type of allowed slots.
    pub allowed_slots: BabeAllowedSlots,
}

/// An consensus log item for BABE.
#[derive(Debug, Decode, Encode, Clone, PartialEq, Eq)]
pub enum ConsensusLog {
    /// The epoch has changed. This provides information about the _next_
    /// epoch - information about the _current_ epoch (i.e. the one we've just
    /// entered) should already be available earlier in the chain.
    #[codec(index = "1")]
    NextEpochData(NextEpochDescriptor),
    /// Disable the authority with given index.
    #[codec(index = "2")]
    OnDisabled(u32),
    /// The epoch has changed, and the epoch after the current one will
    /// enact different epoch configurations.
    #[codec(index = "3")]
    NextConfigData(NextConfigDescriptor),
}

/// Information about the next epoch. This is broadcast in the first block
/// of the epoch.
#[derive(Decode, Encode, PartialEq, Eq, Clone, Debug)]
pub struct NextEpochDescriptor {
    /// The authorities.
    pub authorities: Vec<([u8; 32], u64)>,

    /// The value of randomness to use for the slot-assignment.
    pub randomness: [u8; 32],
}

/// Information about the next epoch config, if changed. This is broadcast in the first
/// block of the epoch, and applies using the same rules as `NextEpochDescriptor`.
#[derive(Decode, Encode, PartialEq, Eq, Clone, Debug)]
pub enum NextConfigDescriptor {
    /// Version 1.
    #[codec(index = "1")]
    V1 {
        /// Value of `c` in `BabeEpochConfiguration`.
        c: (u64, u64),
        /// Value of `allowed_slots` in `BabeEpochConfiguration`.
        allowed_slots: BabeAllowedSlots,
    },
}

/// A BABE pre-runtime digest. This contains all data required to validate a
/// block and for the BABE runtime module. Slots can be assigned to a primary
/// (VRF based) and to a secondary (slot number based).
#[derive(Clone, Debug, Encode, Decode)]
pub enum PreDigest {
    /// A primary VRF-based slot assignment.
    #[codec(index = "1")]
    Primary(PrimaryPreDigest),
    /// A secondary deterministic slot assignment.
    #[codec(index = "2")]
    SecondaryPlain(SecondaryPlainPreDigest),
    /// A secondary deterministic slot assignment with VRF outputs.
    #[codec(index = "3")]
    SecondaryVRF(SecondaryVRFPreDigest),
}

/// Raw BABE primary slot assignment pre-digest.
#[derive(Clone, Encode, Decode)]
pub struct PrimaryPreDigest {
    /// Authority index
    pub authority_index: u32,
    /// Slot number
    pub slot_number: u64,
    /// VRF output
    pub vrf_output: [u8; 32],
    /// VRF proof
    pub vrf_proof: [u8; 64],
}

// This custom Debug implementation exists because `[u8; 64]` doesn't implement `Debug`
impl fmt::Debug for PrimaryPreDigest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("PrimaryPreDigest").finish()
    }
}

/// BABE secondary slot assignment pre-digest.
#[derive(Clone, Debug, Encode, Decode)]
pub struct SecondaryPlainPreDigest {
    /// Authority index
    ///
    /// This is not strictly-speaking necessary, since the secondary slots
    /// are assigned based on slot number and epoch randomness. But including
    /// it makes things easier for higher-level users of the chain data to
    /// be aware of the author of a secondary-slot block.
    pub authority_index: u32,
    /// Slot number
    pub slot_number: u64,
}

/// BABE secondary deterministic slot assignment with VRF outputs.
#[derive(Clone, Encode, Decode)]
pub struct SecondaryVRFPreDigest {
    /// Authority index
    pub authority_index: u32,
    /// Slot number
    pub slot_number: u64,
    /// VRF output
    pub vrf_output: [u8; 32],
    /// VRF proof
    pub vrf_proof: [u8; 64],
}

// This custom Debug implementation exists because `[u8; 64]` doesn't implement `Debug`
impl fmt::Debug for SecondaryVRFPreDigest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("SecondaryVRFPreDigest").finish()
    }
}
