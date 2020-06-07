//! Description of one of the possible entry points of the executor.
//!
//! Contrary to most programs, Wasm runtime code doesn't have a singe `main` function. Instead, it
//! exposes several entry points. Which one to call indicates which action it has to perform. Not
//! all entry points are necessarily available on all runtimes.
//!
//! # ABI
//!
//! All entry points have the same signature:
//!
//! ```ignore
//! (func $runtime_entry(param $data i32) (param $len i32) (result i64))
//! ```
//!
//! In order to call into the runtime, one must write a buffer of data containing the input
//! parameters into the Wasm virtual machine's memory, then pass a pointer and length of this
//! buffer as the parameters of the entry point.
//!
//! The function returns a 64bits number. The 32 less significant bits represent a pointer to the
//! Wasm virtual machine's memory, and the 32 most significant bits a length. This pointer and
//! length designate a buffer containing the actual return value.

// TODO: redefine `Block` locally as a SCALE-encoded opaque blob
use crate::block::Block;

use alloc::vec::Vec;

use parity_scale_codec::{DecodeAll as _, Encode as _};

/// Which function to call on a runtime, and its parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionToCall<'a> {
    CoreVersion,
    CoreExecuteBlock(&'a Block),
    BabeApiConfiguration,
    GrandpaApiGrandpaAuthorities,
    TaggedTransactionsQueueValidateTransaction(&'a [u8]),
    // TODO: add the block builder functions
}

impl<'a> FunctionToCall<'a> {
    /// Splits this object into an entry point and its parameter.
    pub(super) fn into_function_and_param(self) -> (CalledFunction, Vec<u8>) {
        let called_function = CalledFunction::from(&self);

        let encoded = match self {
            FunctionToCall::CoreVersion => Vec::new(),
            FunctionToCall::CoreExecuteBlock(block) => block.encode(),
            FunctionToCall::BabeApiConfiguration => Vec::new(),
            FunctionToCall::GrandpaApiGrandpaAuthorities => Vec::new(),
            FunctionToCall::TaggedTransactionsQueueValidateTransaction(extrinsic) => {
                extrinsic.encode()
            }
        };

        (called_function, encoded)
    }
}

/// Same as [`FunctionToCall`] but without parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CalledFunction {
    CoreVersion,
    CoreExecuteBlock,
    BabeApiConfiguration,
    GrandpaApiGrandpaAuthorities,
    TaggedTransactionsQueueValidateTransaction,
}

impl CalledFunction {
    /// Returns the name of the symbol that the Wasm runtime exports if it provides this entry
    /// point.
    pub fn symbol_name(&self) -> &'static str {
        match self {
            CalledFunction::CoreVersion => "Core_version",
            CalledFunction::CoreExecuteBlock => "Core_execute_block",
            CalledFunction::BabeApiConfiguration => "BabeApi_configuration",
            CalledFunction::GrandpaApiGrandpaAuthorities => "GrandpaApi_grandpa_authorities",
            CalledFunction::TaggedTransactionsQueueValidateTransaction => {
                "TaggedTransactionsQueue_validate_transaction"
            }
        }
    }
}

impl<'a, 'b> From<&'a FunctionToCall<'b>> for CalledFunction {
    fn from(c: &'a FunctionToCall<'b>) -> Self {
        match c {
            FunctionToCall::CoreVersion => CalledFunction::CoreVersion,
            FunctionToCall::CoreExecuteBlock(_) => CalledFunction::CoreExecuteBlock,
            FunctionToCall::BabeApiConfiguration => CalledFunction::BabeApiConfiguration,
            FunctionToCall::GrandpaApiGrandpaAuthorities => {
                CalledFunction::GrandpaApiGrandpaAuthorities
            }
            FunctionToCall::TaggedTransactionsQueueValidateTransaction(_) => {
                CalledFunction::TaggedTransactionsQueueValidateTransaction
            }
        }
    }
}

/// Contains the same variants as [`CalledFunction`] but with the strongly-typed success value.
#[derive(Debug)]
pub enum Success {
    CoreVersion(CoreVersionSuccess),
    CoreExecuteBlock(bool),
    // TODO: other functions
}

impl Success {
    /// After `function` has been called and has written `data` in the Wasm virtual machine, this
    /// function can be used to obtain the corresponding `Success`.
    pub(super) fn decode(
        function: &CalledFunction,
        data: &[u8],
    ) -> Result<Success, SuccessDecodeErr> {
        match function {
            CalledFunction::CoreVersion => {
                Ok(Success::CoreVersion(CoreVersionSuccess::decode_all(data)?))
            }
            CalledFunction::CoreExecuteBlock => {
                Ok(Success::CoreExecuteBlock(bool::decode_all(data)?))
            }
            _ => unimplemented!("return value for this function not implemented"), // TODO:
        }
    }
}

/// Error that can happen when decoding the entry point return value.
#[derive(Debug, Clone, derive_more::Display, derive_more::From)]
pub enum SuccessDecodeErr {
    /// Couldn't decode the data.
    Decode(parity_scale_codec::Error),
}

/// Structure that the `CoreVersion` function returns.
#[derive(Debug, Clone, PartialEq, Eq, parity_scale_codec::Encode, parity_scale_codec::Decode)]
pub struct CoreVersionSuccess {
    pub spec_name: String,
    pub impl_name: String,
    pub authoring_version: u32,
    pub spec_version: u32,
    pub impl_version: u32,
    // TODO: specs document this as an unexplained `ApisVec`; report that to specs writer
    // TODO: stronger typing
    pub apis: Vec<([u8; 8], u32)>,
    // TODO: this field has been introduced recently and isn't mention in the specs; report that to specs writer
    pub transaction_version: u32,
}
