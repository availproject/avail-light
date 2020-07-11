//! BABE consensus.
//!
//! BABE, or Blind Assignment for Blockchain Extension, is the consensus algorithm used by
//! Polkadot in order to determine who is authorized to generate a block.
//!
//! References:
//!
//! - https://research.web3.foundation/en/latest/polkadot/BABE/Babe.html
//!
//!
//! # Overview
//!
//! In the BABE algorithm, time is divided into non-overlapping **epochs**, themselves divided
//! into **slots**.
//! At the beginning of each epoch, .
//! TODO: ^
//!
//! # Claiming a slot
//!
//! The action of "claiming a slot" is performed by producing a block during the interval of time
//! of the slot.
//!

mod definitions;

/// Verifies whether a block header.
pub fn verify_header() {}
