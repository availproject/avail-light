//! Block verification.
//!
//! When we receive a block from the network whose number is higher than the number of the best
//! block that we know of, we can potentially add this block to the chain and treat it as the new
//! best block.
//!
//! But before doing so, we must first verify whether the block is correct. In other words, that
//! all the extrinsics in the block can indeed be applied on top of its parent.
//!
//! Verifying a block consists of twp main steps:
//!
//! - Verifying the consensus layer to make sure that the author of the block was authorized to
//! produce it.
//! - Executing the block. This involves calling the `Core_execute_block` runtime function with
//! the header and body of the block for the runtime to verify that all the extrinsics are
//! correct.
//!

mod unsealed;

pub mod header_body;
pub mod header_only;
