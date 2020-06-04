//! Public/private keys management.
//!
//! The keystore owns a collection of the private keys. Private keys themselves aren't accessible
//! through the keystore's API. Only signing and validating is.
//!
//! The keystore is only responsible for owning private keys, but doesn't necessarily holds the
//! keys themselves in memory. It might for example delegate signing and validating to a hardware
//! wallet.

/// Container for private keys.
pub struct Keystore {}

impl Keystore {
    /// Builds an empty collection.
    pub fn empty() -> Keystore {
        Keystore {}
    }
}
