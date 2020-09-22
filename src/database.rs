//! Persistent data storage.
//!
//! This module contains sub-modules that provide different means of storing data in a
//! persistent way.

pub mod local_storage_light;

// Note for the reader: sled hasn't been chosen by any particular reason other that it is a
// pure Rust database with an image of being robust. There is no strong committment from
// substrate-lite towards sled or any database engine at the moment.
// TODO: remove this node at some point ^
pub mod sled;
