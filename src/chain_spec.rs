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

//! Substrate chain configuration.
//!
//! A **chain spec** (short for *chain specification*) is the description of everything that is
//! required for the client to successfully interact with a certain blockchain.
//! For example, the Polkadot chain spec contains all the constants that are needed in order to
//! successfully interact with Polkadot.
//!
//! Chain specs contain, notably:
//!
//! - The state of the genesis block. In other words, the initial content of the database. This
//! includes the Wasm runtime code of the genesis block.
//! - The list of bootstrap nodes. These are the IP addresses of the machines we need to connect
//! to.
//! - The default telemetry endpoints, to which we should send telemetry information to.
//! - The name of the network protocol, in order to avoid accidentally connecting to a different
//! network.
//! - Multiple other miscellaneous information.
//!

use alloc::string::String;

mod light_sync_state;
mod structs;

/// A configuration of a chain. Can be used to build a genesis block.
#[derive(Clone)]
pub struct ChainSpec {
    client_spec: structs::ClientSpec,
}

impl ChainSpec {
    /// Parse JSON content into a [`ChainSpec`].
    pub fn from_json_bytes(json: impl AsRef<[u8]>) -> Result<Self, ParseError> {
        let client_spec: structs::ClientSpec =
            serde_json::from_slice(json.as_ref()).map_err(ParseError)?;

        if let Some(sync_state) = client_spec.light_sync_state.as_ref() {
            let decoded = sync_state.decode();
            println!("{:?}", decoded);
        }

        // TODO: we don't support child tries in the genesis block
        assert!({
            let structs::Genesis::Raw(genesis) = &client_spec.genesis;
            genesis.children_default.is_empty()
        });
        Ok(ChainSpec { client_spec })
    }

    /// Returns the name of the chain. Meant to be displayed to the user.
    pub fn name(&self) -> &str {
        &self.client_spec.name
    }

    /// Returns the identifier of the chain. Similar to the name, but a bit more "system-looking".
    /// For example, if the name is "Flaming Fir 7", then the id could be "flamingfir7". To be
    /// used for example in file system paths.
    pub fn id(&self) -> &str {
        &self.client_spec.id
    }

    /// Returns a string indicating the type of chain.
    ///
    /// This value doesn't have any meaning in the absolute and is only meant to be shown to
    /// the user.
    pub fn chain_type(&self) -> &str {
        match &self.client_spec.chain_type {
            structs::ChainType::Development => "Development",
            structs::ChainType::Local => "Local",
            structs::ChainType::Live => "Live",
            structs::ChainType::Custom(ty) => ty,
        }
    }

    /// Returns the list of bootnode addresses in the chain specs.
    // TODO: more strongly typed?
    pub fn boot_nodes(&self) -> &[String] {
        &self.client_spec.boot_nodes
    }

    /// Returns the list of libp2p multiaddresses of the default telemetry servers of the chain.
    // TODO: more strongly typed?
    pub fn telemetry_endpoints<'a>(&'a self) -> impl Iterator<Item = impl AsRef<str> + 'a> + 'a {
        self.client_spec
            .telemetry_endpoints
            .as_ref()
            .into_iter()
            .flat_map(|ep| ep.iter().map(|e| &e.0))
    }

    /// Returns the network protocol id that uniquely identifies a chain. Used to prevent nodes
    /// from different blockchain networks from accidentally connecting to each other.
    ///
    /// It is possible for the JSON chain specs to not specify any protocol id, in which case a
    /// default value is returned.
    pub fn protocol_id(&self) -> &str {
        self.client_spec
            .protocol_id
            .as_ref()
            .map(String::as_str)
            .unwrap_or("sup")
    }

    /// Returns the list of storage keys and values of the genesis block.
    pub fn genesis_storage(&self) -> impl ExactSizeIterator<Item = (&[u8], &[u8])> + Clone {
        let structs::Genesis::Raw(genesis) = &self.client_spec.genesis;
        genesis.top.iter().map(|(k, v)| (&k.0[..], &v.0[..]))
    }

    /// Returns a list of arbitrary properties contained in the chain specs, such as the name of
    /// the token or the number of decimals.
    ///
    /// The value of these properties is never interpreted by the local node, but can be served
    /// to a UI.
    ///
    /// The returned value is a JSON-formatted map, for example `{"foo":"bar"}`.
    pub fn properties(&self) -> &str {
        self.client_spec
            .properties
            .as_ref()
            .map(|p| p.get())
            .unwrap_or("{}")
    }
}

/// Error that can happen when parsing a chain spec JSON.
#[derive(Debug, derive_more::Display)]
pub struct ParseError(serde_json::Error);

#[cfg(test)]
mod tests {
    use super::ChainSpec;

    #[test]
    fn can_decode_polkadot_genesis() {
        let spec = &include_bytes!("chain_spec/example.json")[..];
        let specs = ChainSpec::from_json_bytes(&spec).unwrap();
        assert_eq!(specs.id(), "polkadot");
    }
}
