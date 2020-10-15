// Copyright (C) 2019-2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

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
