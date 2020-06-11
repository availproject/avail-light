//! Information string printed out.
//!
//! The so-called informant is a line of text typically printed at a regular interval on stdout.
//!
//! You can make the informant overwrite itself by printing only a `\r` at the end.
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
//! eprint!("{}", InformantLine {
//!     num_connected_peers: 12,
//!     best_number: 220,
//!     finalized_number: 217,
//!     best_hash: &[0x12, 0x34, 0x56, 0x76],
//!     finalized_hash: &[0xaa, 0xbb, 0xcc, 0xdd],
//! });
//! ```

use ansi_term::Colour; // TODO: this crate isn't no_std
use core::{cmp, convert::TryFrom as _, fmt, iter};

/// Values used to build the informant line. Implements the [`core::fmt::Display`] trait.
#[derive(Debug)]
pub struct InformantLine<'a> {
    // TODO: pub enable_colors: bool,
    /// Name of the chain.
    pub chain_name: &'a str,
    /// Maximum number of characters of the informant line.
    pub max_line_width: u32,
    pub num_network_connections: u64,
    pub best_number: u64,
    pub finalized_number: u64,
    pub best_hash: &'a [u8],
    pub finalized_hash: &'a [u8],
    pub network_known_best: u64,
}

impl<'a> fmt::Display for InformantLine<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: lots of allocations in here
        // TODO: this bar is actually harder to implement than expected

        let header_unaligned = format!(
            "{chain_name}   #{local_best}",
            chain_name = Colour::Cyan.paint(self.chain_name),
            local_best = Colour::White
                .bold()
                .paint(&format!("{:<7}", self.best_number)),
        );

        let header = format!("  {} [", header_unaligned);
        let header_len = self.chain_name.chars().count() + 15; // TODO: ? it's easier to do that than deal with unicode

        // TODO: it's a bit of a clusterfuck to properly align because the emoji eats a whitespace
        let trailer = format!(
            "] #{network_best} (🌐{connec}) ",
            network_best = Colour::White
                .bold()
                .paint(&self.network_known_best.to_string()),
            connec = Colour::White
                .bold()
                .paint(format!("{:>4}", self.num_network_connections)),
        );
        let trailer_len = format!(
            "] #{network_best} (  {connec}) ",
            network_best = self.network_known_best,
            connec = format!("{:>4}", self.num_network_connections),
        )
        .len();

        let bar_width = self
            .max_line_width
            .saturating_sub(u32::try_from(header_len).unwrap())
            .saturating_sub(u32::try_from(trailer_len).unwrap());

        let actual_network_best = cmp::max(self.network_known_best, self.best_number);
        assert!(self.best_number <= actual_network_best);
        let bar_done_width = u128::from(self.best_number)
            .checked_mul(u128::from(bar_width))
            .unwrap()
            .checked_div(u128::from(actual_network_best))
            .unwrap_or(0); // TODO: hack to not panic
        let bar_done_width = u32::try_from(bar_done_width).unwrap();

        let done_bar1 = iter::repeat('=')
            .take(usize::try_from(bar_done_width.saturating_sub(1)).unwrap())
            .collect::<String>();
        let done_bar2 = if bar_done_width == bar_width {
            '='
        } else {
            '>'
        };
        let todo_bar = iter::repeat(' ')
            .take(
                usize::try_from(
                    bar_width
                        .checked_sub(bar_done_width.saturating_sub(1).saturating_add(1))
                        .unwrap(),
                )
                .unwrap(),
            )
            .collect::<String>();
        assert_eq!(
            done_bar1.len() + 1 + todo_bar.len(),
            usize::try_from(bar_width).unwrap()
        );

        write!(
            f,
            "{header}{done_bar1}{done_bar2}{todo_bar}{trailer}",
            header = header,
            done_bar1 = done_bar1,
            done_bar2 = done_bar2,
            todo_bar = todo_bar,
            trailer = trailer
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
