//! Information string printed out.
//!
//! The so-called informant is a line of text typically printed at a regular interval on stdout.
//!
//! This code intentionally doesn't perform any printing and only provides some helping code to
//! make the printing straight-forward.
//!
//! # Usage
//!
//! The [`InformantLine`] struct implements the [`core::fmt::Display`] trait.
//!
//! ```
//! use substrate_lite::informant::InformantLine;
//! eprintln!("{}", InformantLine {
//!     num_connected_peers: 12,
//!     best_number: 220,
//!     finalized_number: 217,
//!     best_hash: &[0x12, 0x34, 0x56, 0x76],
//!     finalized_hash: &[0xaa, 0xbb, 0xcc, 0xdd],
//! });
//! ```

use ansi_term::Colour; // TODO: this crate isn't no_std
use core::{convert::TryFrom as _, fmt};

/// Values shown on the informant line. Implements the [`core::fmt::Display`] trait.
#[derive(Debug)]
pub struct InformantLine<'a> {
    // TODO: pub enable_colors: bool,
    pub num_connected_peers: u32,
    pub best_number: u64,
    pub finalized_number: u64,
    pub best_hash: &'a [u8],
    pub finalized_hash: &'a [u8],
}

impl<'a> fmt::Display for InformantLine<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = Colour::White.bold().paint("💤 Idle");

        write!(
            f,
            "{} ({} peers), best: #{} ({}), finalized #{} ({})",
            status,
            Colour::White
                .bold()
                .paint(self.num_connected_peers.to_string()), // TODO: pretty printing
            Colour::White.bold().paint(&self.best_number.to_string()),
            HashDisplay(self.best_hash),
            Colour::White
                .bold()
                .paint(&self.finalized_number.to_string()), // TODO: pretty printing
            HashDisplay(self.finalized_hash),
        )
    }
}

/// Implements `fmt::Display` and displays hashes in a nice way.
struct HashDisplay<'a>(&'a [u8]);

impl<'a> fmt::Display for HashDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x")?;
        if self.0.len() >= 2 {
            let val = u16::from_be_bytes(<[u8; 2]>::try_from(&self.0[..2]).unwrap());
            write!(f, "{:04x}", val)?;
        }
        if self.0.len() >= 5 {
            write!(f, "…")?;
        }
        if self.0.len() >= 4 {
            let len = self.0.len();
            let val = u16::from_be_bytes(<[u8; 2]>::try_from(&self.0[len - 2..]).unwrap());
            write!(f, "{:04x}", val)?;
        }
        Ok(())
    }
}
