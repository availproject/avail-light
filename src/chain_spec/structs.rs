// Substrate-lite
// Copyright (C) 2019-2020  Parity Technologies (UK) Ltd.
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

//! Type definitions that implement the [`serde::Serialize`] and [`serde::Deserialize`] traits and
//! that match the chain specs JSON file structure.
//!
//! The main type is [`ClientSpec`].

use super::light_sync_state::LightSyncState;
use fnv::FnvBuildHasher;
use hashbrown::{HashMap, HashSet};
use primitive_types::H256;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(super) struct ClientSpec {
    pub(super) name: String,
    pub(super) id: String,
    #[serde(default)]
    pub(super) chain_type: ChainType,
    pub(super) boot_nodes: Vec<String>,
    pub(super) telemetry_endpoints: Option<Vec<(String, u8)>>,
    pub(super) protocol_id: Option<String>,
    pub(super) properties: Option<Box<serde_json::value::RawValue>>,
    // TODO: make use of this
    pub(super) fork_blocks: Option<Vec<(u64, H256)>>,
    // TODO: make use of this
    pub(super) bad_blocks: Option<HashSet<H256, FnvBuildHasher>>,
    // Unused but for some reason still part of the chain specs.
    pub(super) consensus_engine: (),
    // TODO: looks deprecated?
    pub(super) genesis: Genesis,
    pub(super) light_sync_state: Option<LightSyncState>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) enum ChainType {
    Development,
    Local,
    Live,
    Custom(String),
}

impl Default for ChainType {
    fn default() -> Self {
        Self::Live
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(super) enum Genesis {
    Raw(RawGenesis),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(super) struct RawGenesis {
    pub(super) top: HashMap<StorageKey, StorageData, FnvBuildHasher>,
    pub(super) children_default: HashMap<StorageKey, ChildRawStorage, FnvBuildHasher>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StorageKey(#[serde(with = "impl_serde::serialize")] pub(super) Vec<u8>);

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StorageData(#[serde(with = "impl_serde::serialize")] pub(super) Vec<u8>);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(super) struct ChildRawStorage {
    pub(super) child_info: Vec<u8>,
    pub(super) child_type: u32,
}
