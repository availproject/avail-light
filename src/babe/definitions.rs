//! Definitions copy-pasted from Substrate

// TODO: decide what to do with this module; it's obviously not great

use parity_scale_codec::{Decode, Encode};

/// BABE epoch information
#[derive(Decode, Encode, PartialEq, Eq, Clone, Debug)]
pub struct Epoch {
    /// The epoch index.
    pub epoch_index: u64,
    /// The starting slot of the epoch.
    pub start_slot: u64,
    /// The duration of this epoch.
    pub duration: u64,
    /// Public keys of the authorities and their weights.
    pub authorities: Vec<([u8; 32], u64)>,
    /// Randomness for this epoch.
    pub randomness: [u8; 32],
    /// Configuration of the epoch.
    pub config: BabeEpochConfiguration,
}

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
    pub allowed_slots: AllowedSlots,
}

/// Configuration data used by the BABE consensus engine.
// TODO: merge with Epoch struct?
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct BabeEpochConfiguration {
    /// A constant value that is used in the threshold calculation formula.
    /// Expressed as a rational where the first member of the tuple is the
    /// numerator and the second is the denominator. The rational should
    /// represent a value between 0 and 1.
    /// In the threshold formula calculation, `1 - c` represents the probability
    /// of a slot being empty.
    pub c: (u64, u64),

    /// Whether this chain should run with secondary slots, which are assigned
    /// in round-robin manner.
    pub allowed_slots: AllowedSlots,
}

/// Types of allowed slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum AllowedSlots {
    /// Only allow primary slots.
    PrimarySlots,
    /// Allow primary and secondary plain slots.
    PrimaryAndSecondaryPlainSlots,
    /// Allow primary and secondary VRF slots.
    PrimaryAndSecondaryVRFSlots,
}
