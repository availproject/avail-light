// Smoldot
// Copyright (C) 2019-2021  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Information string printed out.
//!
//! The so-called informant is a line of text typically printed at a regular interval on stdout.
//!
//! You can make the informant overwrite itself by printing a `\r` at the end of it.
//!
//! This code intentionally doesn't perform any printing and only provides some helping code to
//! make the printing straight-forward.
//!
//! # Usage
//!
//! The [`InformantLine`] struct implements the [`core::fmt::Display`] trait.
//!
//! ```
//! use smoldot::informant::InformantLine;
//! eprint!("{}\r", InformantLine {
//!     enable_colors: true,
//!     chain_name: "My chain",
//!     relay_chain: None,
//!     max_line_width: 80,
//!     num_network_connections: 12,
//!     best_number: 220,
//!     finalized_number: 217,
//!     best_hash: &[0x12, 0x34, 0x56, 0x76],
//!     finalized_hash: &[0xaa, 0xbb, 0xcc, 0xdd],
//!     network_known_best: Some(224),
//! });
//! ```

use alloc::{format, string::String};
use core::{cmp, convert::TryFrom as _, fmt, iter};

/// Values used to build the informant line. Implements the [`core::fmt::Display`] trait.
// TODO: some fields here aren't printed; remove them once what is printed is final
#[derive(Debug)]
pub struct InformantLine<'a> {
    /// If true, ANSI escape characters will be written out.
    ///
    /// > **Note**: Despite its name, this controls *all* styling escape characters, not just
    /// >           colors.
    pub enable_colors: bool,
    /// Name of the chain.
    pub chain_name: &'a str,
    /// Extra fields related to the relay chain.
    pub relay_chain: Option<RelayChain<'a>>,
    /// Maximum number of characters of the informant line.
    pub max_line_width: u32,
    /// Number of network connections we are having with the rest of the peer-to-peer network.
    pub num_network_connections: u64,
    /// Best block currently being propagated on the peer-to-peer. `None` if unknown.
    pub network_known_best: Option<u64>,
    /// Number of the best block that we have locally.
    pub best_number: u64,
    /// Hash of the best block that we have locally.
    pub best_hash: &'a [u8],
    /// Number of the latest finalized block we have locally.
    pub finalized_number: u64,
    /// Hash of the latest finalized block we have locally.
    pub finalized_hash: &'a [u8],
}

/// Extra fields if a relay chain exists.
#[derive(Debug)]
pub struct RelayChain<'a> {
    /// Name of the chain.
    pub chain_name: &'a str,
    /// Number of the best block that we have locally.
    pub best_number: u64,
}

impl<'a> fmt::Display for InformantLine<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: lots of allocations in here
        // TODO: this bar is actually harder to implement than expected; clean up code

        // Define the escape sequences used below for colouring purposes.
        let cyan = if self.enable_colors { "\x1b[36m" } else { "" };
        let white_bold = if self.enable_colors { "\x1b[1;37m" } else { "" };
        let light_gray = if self.enable_colors { "\x1b[90m" } else { "" };
        let reset = if self.enable_colors { "\x1b[0m" } else { "" };

        let (header, header_len) = if let Some(relay_chain) = &self.relay_chain {
            let header = format!(
                "    {cyan}{chain_name}{reset}   {white_bold}#{local_best:<7}{reset} {light_gray}({relay_chain_name} #{relay_best}){reset} [",
                cyan = cyan,
                reset = reset,
                white_bold = white_bold,
                light_gray = light_gray,
                chain_name = self.chain_name,
                relay_chain_name = relay_chain.chain_name,
                local_best = self.best_number,
                relay_best = relay_chain.best_number,
            );

            let header_len = self.chain_name.chars().count() + relay_chain.chain_name.len() + 27; // TODO: ? it's easier to do that than deal with unicode
            (header, header_len)
        } else {
            let header = format!(
                "    {cyan}{chain_name}{reset}   {white_bold}#{local_best:<7}{reset} [",
                cyan = cyan,
                reset = reset,
                white_bold = white_bold,
                chain_name = self.chain_name,
                local_best = self.best_number,
            );

            let header_len = self.chain_name.chars().count() + 17; // TODO: ? it's easier to do that than deal with unicode
            (header, header_len)
        };

        // TODO: it's a bit of a clusterfuck to properly align because the emoji eats a whitespace
        let trailer = format!(
            "] {white_bold}#{network_best}{reset} (🌐{white_bold}{connec:>4}{reset})   ",
            network_best = self.network_known_best.unwrap_or(0),
            connec = self.num_network_connections,
            white_bold = white_bold,
            reset = reset,
        );
        let trailer_len = format!(
            "] #{network_best} (  {connec:>4})   ",
            network_best = self.network_known_best.unwrap_or(0),
            connec = self.num_network_connections,
        )
        .len();

        let bar_width = self
            .max_line_width
            .saturating_sub(u32::try_from(header_len).unwrap())
            .saturating_sub(u32::try_from(trailer_len).unwrap());

        let actual_network_best = cmp::max(self.network_known_best.unwrap_or(0), self.best_number);
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
