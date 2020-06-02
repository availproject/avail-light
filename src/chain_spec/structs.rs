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

// TODO: change visibility to pub(super) everywhere, once we no longer leak types

use fnv::FnvBuildHasher;
use hashbrown::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use primitive_types::H256;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildRawStorage {
    pub(crate) child_info: Vec<u8>,
    pub(crate) child_type: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
/// Storage content for genesis block.
pub(crate) struct RawGenesis {
    pub(crate) top: HashMap<StorageKey, StorageData, FnvBuildHasher>,
    pub(crate) children_default: HashMap<StorageKey, ChildRawStorage, FnvBuildHasher>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) enum Genesis {
    Raw(RawGenesis),
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StorageKey(#[serde(with = "impl_serde::serialize")] pub(crate) Vec<u8>);

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StorageData(#[serde(with = "impl_serde::serialize")] pub(crate) Vec<u8>);

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StorageChangeSet<Hash> {
    /// Block hash
    pub(crate) block: Hash,
    /// A list of changes
    pub(crate) changes: Vec<(StorageKey, Option<StorageData>)>,
}

/// A configuration of a client. Does not include runtime storage initialization.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientSpec {
    pub(crate) name: String,
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) chain_type: ChainType,
    pub(crate) boot_nodes: Vec<String>,
    pub(crate) telemetry_endpoints: Option<TelemetryEndpoints>,
    pub(crate) protocol_id: Option<String>,
    pub(crate) properties: Option<Properties>,
    pub(crate) fork_blocks: Option<Vec<(u64, H256)>>,
    pub(crate) bad_blocks: Option<HashSet<H256>>,
    // Unused but for some reason still part of the chain specs.
    pub(crate) consensus_engine: (),
    // TODO: looks deprecated?
    pub(crate) genesis: Genesis,
}

/// The type of a chain.
///
/// This can be used by tools to determine the type of a chain for displaying
/// additional information or enabling additional features.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ChainType {
    /// A development chain that runs mainly on one node.
    Development,
    /// A local chain that runs locally on multiple nodes for testing purposes.
    Local,
    /// A live chain.
    Live,
    /// Some custom chain type.
    Custom(String),
}

impl Default for ChainType {
    fn default() -> Self {
        Self::Live
    }
}

/// List of telemetry servers we want to talk to. Contains the URL of the server, and the
/// maximum verbosity level.
///
/// The URL string can be either a URL or a multiaddress.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct TelemetryEndpoints(Vec<(String, u8)>);

/// Arbitrary properties defined in chain spec as a JSON object
pub(crate) type Properties = serde_json::map::Map<String, serde_json::Value>;
