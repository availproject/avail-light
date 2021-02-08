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

//! WebAssembly runtime code execution.
//!
//! WebAssembly (often abbreviated *Wasm*) plays a big role in Substrate/Polkadot. The storage of
//! each block in the chain has a special key named `:code` which contains the WebAssembly code
//! of what we call *the runtime*.
//!
//! The runtime is a program (in WebAssembly) that decides, amongst other things, whether
//! transactions are valid and how to apply them on the storage, and whether blocks themselves are
//! valid.
//!
//! This module contains everything necessary to execute runtime code. The highest-level
//! sub-module is [`runtime_host`].

use alloc::vec::Vec;
use core::{convert::TryFrom as _, str};

mod allocator; // TODO: make public after refactoring
pub mod host;
pub mod read_only_runtime_host;
pub mod runtime_host;
pub mod vm;

/// Default number of heap pages if the storage doesn't specify otherwise.
///
/// # Context
///
/// In order to initialize a [`host::HostVmPrototype`], one needs to pass a certain number of
/// heap pages that are available to the runtime.
///
/// This number is normally found in the storage, at the key `:heappages`. But if it is not
/// specified, then the value of this constant must be used.
pub const DEFAULT_HEAP_PAGES: u64 = 1024;

/// Runs the `Core_version` function using the given virtual machine prototype, and returns
/// the output.
///
/// All externalities are forbidden.
// TODO: proper error
pub fn core_version(
    vm_proto: host::HostVmPrototype,
) -> Result<(CoreVersion, host::HostVmPrototype), ()> {
    // TODO: is there maybe a better way to handle that?
    let mut vm: host::HostVm = vm_proto
        .run_no_param("Core_version")
        .map_err(|_| ())?
        .into();

    loop {
        match vm {
            host::HostVm::ReadyToRun(r) => vm = r.run(),
            host::HostVm::Finished(finished) => {
                let _ = decode(&finished.value().as_ref())?;
                let version = finished.value().as_ref().to_vec();
                return Ok((CoreVersion(version), finished.into_prototype()));
            }
            host::HostVm::Error { .. } => return Err(()),
            host::HostVm::LogEmit(log) => vm = log.resume(),

            // Since there are potential ambiguities we don't allow any storage access
            // or anything similar. The last thing we want is to have an infinite
            // recursion of runtime calls.
            _ => return Err(()),
        }
    }
}

/// Buffer storing the SCALE-encoded core version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreVersion(Vec<u8>);

impl CoreVersion {
    pub fn decode(&self) -> CoreVersionRef {
        decode(&self.0).unwrap()
    }
}

impl AsRef<[u8]> for CoreVersion {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Runtime specifications, once decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreVersionRef<'a> {
    pub spec_name: &'a str,
    pub impl_name: &'a str,
    pub authoring_version: u32,
    pub spec_version: u32,
    pub impl_version: u32,
    // TODO: stronger typing, and don't use a Vec
    pub apis: Vec<([u8; 8], u32)>,
    /// `None` if the field is missing.
    pub transaction_version: Option<u32>,
}

fn decode(scale_encoded: &[u8]) -> Result<CoreVersionRef, ()> {
    let result = nom::combinator::all_consuming(nom::combinator::map(
        nom::sequence::tuple((
            string_decode,
            string_decode,
            nom::number::complete::le_u32,
            nom::number::complete::le_u32,
            nom::number::complete::le_u32,
            nom::combinator::flat_map(crate::util::nom_scale_compact_usize, |num_elems| {
                nom::multi::many_m_n(
                    num_elems,
                    num_elems,
                    nom::combinator::map(
                        nom::sequence::tuple((
                            nom::bytes::complete::take(8u32),
                            nom::number::complete::le_u32,
                        )),
                        move |(hash, version)| (<[u8; 8]>::try_from(hash).unwrap(), version),
                    ),
                )
            }),
            nom::branch::alt((
                nom::combinator::map(nom::number::complete::le_u32, Some),
                nom::combinator::map(nom::combinator::eof, |_| None),
            )),
        )),
        |(
            spec_name,
            impl_name,
            authoring_version,
            spec_version,
            impl_version,
            apis,
            transaction_version,
        )| CoreVersionRef {
            spec_name,
            impl_name,
            authoring_version,
            spec_version,
            impl_version,
            apis,
            transaction_version,
        },
    ))(scale_encoded);

    match result {
        Ok((_, out)) => Ok(out),
        Err(nom::Err::Error(_)) | Err(nom::Err::Failure(_)) => Err(()),
        Err(_) => unreachable!(),
    }
}

fn string_decode<'a>(bytes: &'a [u8]) -> nom::IResult<&'a [u8], &'a str> {
    nom::combinator::map_res(
        nom::multi::length_data(crate::util::nom_scale_compact_usize),
        str::from_utf8,
    )(bytes)
}
