//! Runtime-provided metadata
//!
//! From the point of the view of the Substrate/Polkadot client, the runtime is a program
//! compiled to WebAssembly that provides a certain list of entry points and has access to a
//! *storage* (provided by the client) as a way to hold information.
//!
//! In order to be able to query an information, for example the amount of tokens present on a
//! certain account, the client can directly read the storage rather than having to enter the
//! WebAssembly code. This is where the *metadata* comes into play.
//!
//! The *metadata* is a collection of data provided by the runtime and that contains useful
//! information to the client, such as:
//!
//! - A list of storage keys whose value contains information that might be useful to the client.
//! - A list of calls that can be performed by emitting transactions.
//! - A list of *events* that can happen in a block, such as a new account.
//! - ...
//!
//! In order to obtain the metadata, a call to an entry point of the runtime code is necessary.
//! Afterwards, the retreived metadata is guaranteed to not change until the runtime code
//! changes.
//!
//! See also:
//! - https://substrate.dev/docs/en/knowledgebase/runtime/metadata
//!

pub mod decode;
mod query;

pub use query::*;

/// Decodes the given SCALE-encoded metadata.
pub fn decode(scale_encoded_metadata: &[u8]) -> Result<decode::MetadataRef, decode::DecodeError> {
    decode::decode(scale_encoded_metadata)
}
