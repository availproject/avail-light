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

mod structs;

/// A configuration of a chain. Can be used to build a genesis block.
#[derive(Clone)]
pub struct ChainSpec {
    client_spec: structs::ClientSpec,
}

impl ChainSpec {
    /// Parse JSON content into a [`ChainSpec`].
    pub fn from_json_bytes(json: impl AsRef<[u8]>) -> Result<Self, serde_json::Error> {
        let client_spec: structs::ClientSpec = serde_json::from_slice(json.as_ref())?;
        // TODO: we don't support child tries in the genesis block
        assert!({
            let structs::Genesis::Raw(genesis) = &client_spec.genesis;
            genesis.children_default.is_empty()
        });
        Ok(ChainSpec { client_spec })
    }

    /// Name of the chain that is specified.
    pub fn name(&self) -> &str {
        &self.client_spec.name
    }

    /// Spec id. This is similar to the name, but a bit more "system-looking". For example, if the
    /// name is "Flaming Fir 7", then the id could be "flamingfir7". To be used in file system
    /// paths for example.
    pub fn id(&self) -> &str {
        &self.client_spec.id
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

    /// Network protocol id. Used to prevent nodes from multiple networks from connecting with each
    /// other. Returns `None` if the chain specs don't specify any.
    pub fn protocol_id(&self) -> Option<&str> {
        self.client_spec.protocol_id.as_ref().map(String::as_str)
    }

    /// Returns the list of storage keys and values of the genesis block.
    pub fn genesis_storage(&self) -> impl ExactSizeIterator<Item = (&[u8], &[u8])> + Clone {
        let structs::Genesis::Raw(genesis) = &self.client_spec.genesis;
        genesis.top.iter().map(|(k, v)| (&k.0[..], &v.0[..]))
    }

    /// The chain specs contain a list of arbitrary properties, such as the name of the token
    /// of the number of decimals.
    /// The values of these properties is never interpreted by the local node, but they are
    /// usually served through the RPC node.
    pub fn properties(&self) -> impl Iterator<Item = (&str, &serde_json::Value)> {
        self.client_spec
            .properties
            .as_ref()
            .map(|p| p.iter().map(|(k, v)| (k.as_ref(), v)))
            .into_iter()
            .flatten()
    }
}

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
