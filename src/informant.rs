//! Information string printed out.
//!
//! The so-called informant is a line of text typically printed at a regular interval on stdout.
//!
//! This code intentionally doesn't perform any printing and only provides some helping code to
//! make the printing straight-forward.

use ansi_term::Colour; // TODO: this crate isn't no_std
use core::fmt;
use primitive_types::H256;

pub struct InformantLine<'a> {
    pub num_connected_peers: u32,
    pub best_number: u64,
    pub finalized_number: u32,
    pub best_hash: &'a H256,
    pub finalized_hash: &'a H256,
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
                .paint(self.num_connected_peers.to_string()),
            Colour::White.bold().paint(&self.best_number.to_string()),
            self.best_hash,
            Colour::White
                .bold()
                .paint(&self.finalized_number.to_string()),
            self.finalized_hash,
        )
    }
}
