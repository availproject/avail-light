//! Justifications contain a proof of the finality of a block.
//!
//! In order to finalize a block, GrandPa authorities must emit votes, also called pre-commits.
//! We consider a block finalized when more than two thirds of the authorities have voted for that
//! block (or one of its descendants) to be finalized.
//!
//! A justification contains all the votes that have been used to prove that a certain block has
//! been finalized. Its purpose is to later be sent to a third party in order to prove this
//! finality.
//!
//! When a justification is received from a third party, it must first be verified. See the
//! [`verify`] module.

pub mod decode;
pub mod verify;
