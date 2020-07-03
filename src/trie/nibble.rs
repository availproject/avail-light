use core::{convert::TryFrom, fmt};

/// A single nibble with four bits.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nibble(u8);

impl TryFrom<u8> for Nibble {
    type Error = NibbleFromU8Error;

    fn try_from(val: u8) -> Result<Self, Self::Error> {
        if val < 16 {
            Ok(Nibble(val))
        } else {
            Err(NibbleFromU8Error::TooLarge)
        }
    }
}

impl From<Nibble> for u8 {
    fn from(nibble: Nibble) -> u8 {
        nibble.0
    }
}

impl fmt::Debug for Nibble {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

/// Error when building a [`Nibble`] from a `u8`.
#[derive(Debug, derive_more::Display)]
pub enum NibbleFromU8Error {
    /// The integer value is too large.
    #[display(fmt = "Value is too large")]
    TooLarge,
}
