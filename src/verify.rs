//! Methods that verify whether a block is correct.
//!
//! # Context
//!
//! When we receive a block from the network whose number is higher than the number of the best
//! block that we know of, we can potentially add this block to the chain and treat it as the new
//! best block.
//!
//! But before doing so, we must first verify whether the block is correct. In other words, that
//! it was legitimately and correctly crafted.
//!
//! # Header and body, or header only
//!
//! There are two options when it comes to verifying a block:
//!
//! - Verifying only a block header.
//! - Verifying both a block header and its body.
//!
//! > **Note**: The option of verifying only the header of a block, and not its body, is
//! >           normally provided for situations where a block's body is simple not known. If
//! >           the block's body is known, it is usually assumed that its validity be checked
//! >           too.
//!
//! Verifying the header of a block consists in verifying its signature in order to make sure
//! that the author of the block was authorized to produce it. Whether the block has been
//! correctly crafted isn't and cannot be verified with only the header.
//!
//! Verifying the body of a block consists, in addition to verifying it header, in *executing*
//! the block. This involves calling the `Core_execute_block` runtime function, passing as
//! parameter the header and body of the block, for the runtime to verify that everything is
//! correct.
//!

mod unsealed;

pub mod header_body;
pub mod header_only;
