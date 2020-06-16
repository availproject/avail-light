// Copyright 2017-2020 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate.  If not, see <http://www.gnu.org/licenses/>.

//! Type definitions that implement the [`serde::Serialize`] and [`serde::Deserialize`] traits and
//! that match the chain specs JSON file structure.
//!
//! The main type is [`ClientSpec`].

// TODO: change visibility to pub(super) everywhere, once we no longer leak types

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
    pub(super) telemetry_endpoints: Option<TelemetryEndpoints>,
    pub(super) protocol_id: Option<String>,
    pub(super) properties: Option<Properties>,
    pub(super) fork_blocks: Option<Vec<(u64, H256)>>,
    pub(super) bad_blocks: Option<HashSet<H256>>,
    // Unused but for some reason still part of the chain specs.
    pub(super) consensus_engine: (),
    // TODO: looks deprecated?
    pub(super) genesis: Genesis,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct TelemetryEndpoints(Vec<(String, u8)>);

pub(super) type Properties = serde_json::map::Map<String, serde_json::Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(super) enum Genesis {
    Raw(RawGenesis),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct RawGenesis {
    pub(crate) top: HashMap<StorageKey, StorageData, FnvBuildHasher>,
    pub(crate) children_default: HashMap<StorageKey, ChildRawStorage, FnvBuildHasher>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StorageKey(#[serde(with = "impl_serde::serialize")] pub(crate) Vec<u8>);

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StorageData(#[serde(with = "impl_serde::serialize")] pub(crate) Vec<u8>);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildRawStorage {
    pub(crate) child_info: Vec<u8>,
    pub(crate) child_type: u32,
}
