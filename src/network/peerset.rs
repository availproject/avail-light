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

//! Data structure storing a networking state. Helper for managing connectivity to overlay
//! networks.
//!
//! The [`Peerset`] is a data structure that holds a list of node identities ([`PeerId`]s) and a
//! list of overlay networks. Each [`PeerId`] is associated with:
//!
//! - A list of active inbound connections.
//! - A list of [`Multiaddr`]s onto which the node is believed to be reachable.
//!   - For each multiaddr, optionally an active connection or pending dialing attempt.
//! - A list of overlay networks the node is believed to belong to.
//!   - For each overlay network the node belongs to, TODO
//!
//! > **Note**: The [`Peerset`] does *do* anything by itself, such as opening new connections. It
//! >           is purely a data structure that helps organize and maintain information about the
//! >           network.
//!
//! # Usage
//!
//! The [`Peerset`] must be initialized with a list of overlay networks the node is interested in.
//!
//! It is assumed that some discovery mechanism, not covered by this module, is in place in order
//! to discover the identities and addresses of nodes that belong to these various overlay
//! networks.
//!

// TODO: finish documentation

use crate::network::peer_id::PeerId;

use ahash::RandomState;
use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};
use core::mem;
use hashbrown::HashMap;
use parity_multiaddr::Multiaddr;
use rand::{seq::IteratorRandom as _, RngCore as _, SeedableRng as _};

/// Configuration for a [`Peerset`].
#[derive(Debug)]
pub struct Config {
    /// Capacity to reserve for containers having a number of peers.
    pub peers_capacity: usize,

    /// Number of overlay networks managed by the [`Peerset`]. The overlay networks are numbered
    /// from 0 to this value excluded.
    pub num_overlay_networks: usize,

    /// Seed for the randomness used to decide how peers are chosen.
    pub randomness_seed: [u8; 32],
}

/// See the [module-level documentation](self).
pub struct Peerset<TPeer, TConn, TPending> {
    /// Same as [`Config::num_overlay_networks`].
    num_overlay_networks: usize,

    peer_ids: HashMap<PeerId, usize, RandomState>,

    peers: slab::Slab<Peer<TPeer>>,

    /// Active and pending connections.
    connections: slab::Slab<Connection<TConn, TPending>>,

    /// PRNG used to randomly select nodes.
    rng: rand_chacha::ChaCha20Rng,

    /// Container that holds tuples of `(peer_index, connection_index)`. Contains the combinations
    /// of connections associated to a certain peer.
    peer_connections: BTreeSet<(usize, usize)>,

    /// Container that holds tuples of `(overlay_index, peer_index)`.
    /// Contains combinations where the peer belongs to the overlay network.
    overlay_peers: BTreeSet<(usize, usize)>,

    /// Container that holds tuples of `(peer_index, overlay_index)`.
    /// Contains combinations where the peer belongs to the overlay network.
    peers_overlays: BTreeSet<(usize, usize)>,

    /// Container that holds tuples of `(connection_index, overlay_index, direction)`.
    connection_overlays: BTreeMap<(usize, usize, SubstreamDirection), SubstreamState>,
}

struct Peer<TPeer> {
    peer_id: PeerId,
    user_data: TPeer,
    addresses: Vec<Multiaddr>,
    connected: bool,
}

struct Connection<TConn, TPending> {
    peer_index: usize,
    ty: ConnectionTy<TConn, TPending>,
}

enum ConnectionTy<TConn, TPending> {
    Poisoned,
    Connected {
        user_data: TConn,
        inbound: bool,
    },
    Pending {
        user_data: TPending,
        target: Multiaddr,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Hash)]
enum SubstreamDirection {
    In,
    Out,
}

/// > **Note**: There is `Closed` variant, as this corresponds to a lack of entry in the hashmap.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Hash)]
enum SubstreamState {
    Pending,
    Open,
}

impl<TPeer, TConn, TPending> Peerset<TPeer, TConn, TPending> {
    /// Creates a [`Peerset`] with the given configuration.
    pub fn new(config: Config) -> Self {
        let mut rng = rand_chacha::ChaCha20Rng::from_seed(config.randomness_seed);

        let peer_ids = {
            let k0 = rng.next_u64();
            let k1 = rng.next_u64();
            let k2 = rng.next_u64();
            let k3 = rng.next_u64();
            HashMap::with_capacity_and_hasher(
                config.peers_capacity,
                RandomState::with_seeds(k0, k1, k2, k3),
            )
        };

        Peerset {
            num_overlay_networks: config.num_overlay_networks,
            rng,
            peer_ids,
            peers: slab::Slab::with_capacity(config.peers_capacity),
            connections: slab::Slab::with_capacity(config.peers_capacity * 2), // TODO: correct capacity?
            peer_connections: BTreeSet::new(),
            overlay_peers: BTreeSet::new(),
            peers_overlays: BTreeSet::new(),
            connection_overlays: BTreeMap::new(),
        }
    }

    /// Returns the number of established connections in the peerset.
    pub fn num_established_connections(&self) -> usize {
        // TODO: O(n)
        self.connections
            .iter()
            .filter(|(_, c)| matches!(c.ty, ConnectionTy::Connected { .. }))
            .count()
    }

    /// Returns the list of nodes that belong to the given overlay network.
    ///
    /// # Panic
    ///
    /// Panics if `overlay_network_index` is out of range.
    ///
    pub fn overlay_network_nodes(
        &self,
        overlay_network_index: usize,
    ) -> impl Iterator<Item = &PeerId> {
        assert!(overlay_network_index < self.num_overlay_networks);
        self.overlay_peers
            .range((overlay_network_index, 0)..=(overlay_network_index, usize::max_value()))
            .map(move |(_, id)| &self.peers[*id].peer_id)
    }

    /// Returns a random node in the list of nodes that match the following criterias:
    ///
    /// - Peerset has at least one outbound connection towards this node.
    /// - None of the connections towards this node have an outbound substream with the given
    /// overlay network.
    ///
    pub fn random_connected_closed_node(
        &mut self,
        overlay_network_index: usize,
    ) -> Option<NodeMutKnown<TPeer, TConn, TPending>> {
        let peer_connections = &self.peer_connections;
        let connection_overlays = &self.connection_overlays;
        let connections = &self.connections;

        let peer_index = self
            .overlay_peers
            .range((overlay_network_index, 0)..=(overlay_network_index, usize::max_value()))
            .map(|(_, index)| *index)
            .filter(move |peer_index| {
                let mut iter = peer_connections
                    .range((*peer_index, 0)..=(*peer_index, usize::max_value()))
                    .map(|(_, connec_id)| *connec_id);
                if iter
                    .clone()
                    .filter(|connec_id| match connections[*connec_id].ty {
                        ConnectionTy::Connected { inbound, .. } => !inbound,
                        ConnectionTy::Pending { .. } => true,
                        ConnectionTy::Poisoned => unreachable!(),
                    })
                    .next()
                    .is_none()
                {
                    return false;
                }

                !iter.any(|connec_id| {
                    connection_overlays.contains_key(&(
                        connec_id,
                        overlay_network_index,
                        SubstreamDirection::Out,
                    ))
                })
            })
            .choose(&mut self.rng)?;

        Some(NodeMutKnown {
            peerset: self,
            peer_index,
        })
    }

    /// Returns a random node in the list of nodes that match the following criterias:
    ///
    /// - Peerset has no connection nor pending connection towards this node.
    /// - Node belongs to the given overlay network.
    ///
    /// Returns `None` if no such node is available.
    pub fn random_not_connected(
        &mut self,
        overlay_network_index: usize,
    ) -> Option<NodeMutKnown<TPeer, TConn, TPending>> {
        let peer_connections = &self.peer_connections;

        let peer_index = self
            .overlay_peers
            .range((overlay_network_index, 0)..=(overlay_network_index, usize::max_value()))
            .map(|(_, index)| *index)
            .filter(move |peer_index| {
                if peer_connections
                    .range((*peer_index, 0)..=(*peer_index, usize::max_value()))
                    .next()
                    .is_some()
                {
                    return false;
                }
                // TODO: check pending too
                true
            })
            .choose(&mut self.rng)?;

        Some(NodeMutKnown {
            peerset: self,
            peer_index,
        })
    }

    /// Gives access to a pending connection within the [`Peerset`].
    pub fn pending_mut(&mut self, id: PendingId) -> Option<PendingMut<TPeer, TConn, TPending>> {
        if self
            .connections
            .get(id.0)
            .map_or(false, |c| matches!(c.ty, ConnectionTy::Pending { .. }))
        {
            Some(PendingMut { peerset: self, id })
        } else {
            None
        }
    }

    /// Gives access to a connection within the [`Peerset`].
    pub fn connection_mut(
        &mut self,
        id: ConnectionId,
    ) -> Option<ConnectionMut<TPeer, TConn, TPending>> {
        if self.connections.contains(id.0) {
            Some(ConnectionMut { peerset: self, id })
        } else {
            None
        }
    }

    /// Gives access to the state of the node with the given identity.
    pub fn node_mut(&mut self, peer_id: PeerId) -> NodeMut<TPeer, TConn, TPending> {
        if let Some(peer_index) = self.peer_ids.get(&peer_id).cloned() {
            NodeMut::Known(NodeMutKnown {
                peerset: self,
                peer_index,
            })
        } else {
            NodeMut::Unknown(NodeMutUnknown {
                peerset: self,
                peer_id,
            })
        }
    }
}

/// Identifier for a connection in a [`Peerset`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct ConnectionId(usize);

/// Access to a connection in the [`Peerset`].
pub struct ConnectionMut<'a, TPeer, TConn, TPending> {
    peerset: &'a mut Peerset<TPeer, TConn, TPending>,
    id: ConnectionId,
}

impl<'a, TPeer, TConn, TPending> ConnectionMut<'a, TPeer, TConn, TPending> {
    /// Returns the identifier of this connection.
    pub fn id(&self) -> ConnectionId {
        self.id
    }

    /// [`PeerId`] the connection is connected to.
    pub fn peer_id(&self) -> &PeerId {
        let index = self.peerset.connections[self.id.0].peer_index;
        &self.peerset.peers[index].peer_id
    }

    /// Returns true if the connection is inbound.
    pub fn is_inbound(&self) -> bool {
        match self.peerset.connections[self.id.0].ty {
            ConnectionTy::Connected { inbound, .. } => inbound,
            _ => unreachable!(),
        }
    }

    /// Gives access to the user data associated with the connection.
    pub fn user_data_mut(&mut self) -> &mut TConn {
        match &mut self.peerset.connections[self.id.0].ty {
            ConnectionTy::Connected { user_data, .. } => user_data,
            _ => unreachable!(),
        }
    }

    /// Gives access to the user data associated with the connection.
    pub fn into_user_data(self) -> &'a mut TConn {
        match &mut self.peerset.connections[self.id.0].ty {
            ConnectionTy::Connected { user_data, .. } => user_data,
            _ => unreachable!(),
        }
    }

    /// Removes the connection from the data structure.
    pub fn remove(self) -> TConn {
        let connection = self.peerset.connections.remove(self.id.0);
        let _was_in = self
            .peerset
            .peer_connections
            .remove(&(connection.peer_index, self.id.0));
        debug_assert!(_was_in);
        match connection.ty {
            ConnectionTy::Connected { user_data, .. } => user_data,
            _ => unreachable!(),
        }
    }
}

/// Identifier for a pending connection in a [`Peerset`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct PendingId(usize);

/// Access to a connection in the [`Peerset`].
pub struct PendingMut<'a, TPeer, TConn, TPending> {
    peerset: &'a mut Peerset<TPeer, TConn, TPending>,
    id: PendingId,
}

impl<'a, TPeer, TConn, TPending> PendingMut<'a, TPeer, TConn, TPending> {
    /// [`PeerId`] the connection is trying to connect to.
    pub fn peer_id(&self) -> &PeerId {
        let index = self.peerset.connections[self.id.0].peer_index;
        &self.peerset.peers[index].peer_id
    }

    /// Address the connection is trying to reach.
    pub fn address(&self) -> &Multiaddr {
        match &self.peerset.connections[self.id.0].ty {
            ConnectionTy::Pending { target, .. } => target,
            _ => unreachable!(),
        }
    }

    /// Turns this pending connection into an established connection by applying `map` on the
    /// user data.
    pub fn into_established(
        self,
        map: impl FnOnce(TPending) -> TConn,
    ) -> ConnectionMut<'a, TPeer, TConn, TPending> {
        let connec = self.peerset.connections.get_mut(self.id.0).unwrap();
        let old_user_data = match mem::replace(&mut connec.ty, ConnectionTy::Poisoned) {
            ConnectionTy::Pending { user_data, .. } => user_data,
            _ => unreachable!(),
        };
        let new_user_data = map(old_user_data);
        connec.ty = ConnectionTy::Connected {
            user_data: new_user_data,
            inbound: false,
        };
        ConnectionMut {
            peerset: self.peerset,
            id: ConnectionId(self.id.0),
        }
    }

    /// Gives access to the user data associated with the connection.
    pub fn user_data_mut(&mut self) -> &mut TPending {
        match &mut self.peerset.connections[self.id.0].ty {
            ConnectionTy::Pending { user_data, .. } => user_data,
            _ => unreachable!(),
        }
    }

    /// Gives access to the user data associated with the connection.
    pub fn into_user_data(self) -> &'a mut TPending {
        match &mut self.peerset.connections[self.id.0].ty {
            ConnectionTy::Pending { user_data, .. } => user_data,
            _ => unreachable!(),
        }
    }

    /// Removes the pending connection from the data structure.
    pub fn remove(self) -> TPending {
        let connection = self.peerset.connections.remove(self.id.0);
        let _was_in = self
            .peerset
            .peer_connections
            .remove(&(connection.peer_index, self.id.0));
        debug_assert!(_was_in);
        match connection.ty {
            ConnectionTy::Pending { user_data, .. } => user_data,
            _ => unreachable!(),
        }
    }
}

/// Access to a node in the [`Peerset`].
pub enum NodeMut<'a, TPeer, TConn, TPending> {
    /// Node is already known to the data structure.
    Known(NodeMutKnown<'a, TPeer, TConn, TPending>),
    /// Node isn't known by the data structure.
    Unknown(NodeMutUnknown<'a, TPeer, TConn, TPending>),
}

impl<'a, TPeer, TConn, TPending> NodeMut<'a, TPeer, TConn, TPending> {
    /// If [`NodeMut::Unknown`], calls the passed closure in order to obtain a user data and
    /// inserts the node in the data structure.
    pub fn or_insert_with(
        self,
        insert: impl FnOnce() -> TPeer,
    ) -> NodeMutKnown<'a, TPeer, TConn, TPending> {
        match self {
            NodeMut::Known(k) => k,
            NodeMut::Unknown(k) => k.insert(insert()),
        }
    }

    /// Shortcut for `or_insert_with(Default::default)`.
    pub fn or_default(self) -> NodeMutKnown<'a, TPeer, TConn, TPending>
    where
        TPeer: Default,
    {
        self.or_insert_with(Default::default)
    }
}

/// Access to a node is already known to the data structure.
pub struct NodeMutKnown<'a, TPeer, TConn, TPending> {
    peerset: &'a mut Peerset<TPeer, TConn, TPending>,
    peer_index: usize,
}

impl<'a, TPeer, TConn, TPending> NodeMutKnown<'a, TPeer, TConn, TPending> {
    /// Adds in the data structure an inbound connection with this node.
    pub fn add_inbound_connection(&mut self, connection: TConn) -> ConnectionId {
        let index = self.peerset.connections.insert(Connection {
            peer_index: self.peer_index,
            ty: ConnectionTy::Connected {
                user_data: connection,
                inbound: true,
            },
        });

        let _newly_inserted = self
            .peerset
            .peer_connections
            .insert((self.peer_index, index));
        debug_assert!(_newly_inserted);

        ConnectionId(index)
    }

    // TODO: what if Multiaddr isn't in known addresses? do we add it?
    pub fn add_outbound_attempt(&mut self, target: Multiaddr, connection: TPending) -> PendingId {
        let index = self.peerset.connections.insert(Connection {
            peer_index: self.peer_index,
            ty: ConnectionTy::Pending {
                user_data: connection,
                target,
            },
        });

        let _newly_inserted = self
            .peerset
            .peer_connections
            .insert((self.peer_index, index));
        debug_assert!(_newly_inserted);

        PendingId(index)
    }

    /// Returns an iterator to the list of current connections to that node.
    pub fn connections<'b>(&'b self) -> impl Iterator<Item = ConnectionId> + 'b {
        self.peerset.peer_connections
            .range((self.peer_index, 0)..=(self.peer_index, usize::max_value()))
            .map(|(_, i)| *i)
            .filter(move |idx| {
                matches!(self.peerset.connections[*idx].ty, ConnectionTy::Connected { .. })
            })
            .map(ConnectionId)
    }

    /// Returns an iterator to the list of current pending connections to that node.
    pub fn pending_connections<'b>(&'b self) -> impl Iterator<Item = PendingId> + 'b {
        self.peerset.peer_connections
            .range((self.peer_index, 0)..=(self.peer_index, usize::max_value()))
            .map(|(_, i)| *i)
            .filter(move |idx| {
                matches!(self.peerset.connections[*idx].ty, ConnectionTy::Pending { .. })
            })
            .map(PendingId)
    }

    /// Adds an address to the list of addresses the node is reachable through.
    ///
    /// Has no effect if this address is already in the list.
    pub fn add_known_address(&mut self, address: Multiaddr) {
        let list = &mut self.peerset.peers[self.peer_index].addresses;
        if list.iter().any(|a| *a == address) {
            return;
        }

        list.push(address);
    }

    /// Removes an address from the list of known addresses.
    ///
    /// Returns `Ok` if this address was in the list and was removed. Returns `Err` if the address
    /// wasn't in the list.
    pub fn remove_known_address(&mut self, address: &Multiaddr) -> Result<(), ()> {
        let addresses = &mut self.peerset.peers[self.peer_index].addresses;
        if let Some(pos) = addresses.iter().position(|a| a == address) {
            addresses.remove(pos);
            Ok(())
        } else {
            Err(())
        }
    }

    /// Returns an iterator to the list of addresses known for this peer.
    pub fn known_addresses<'b>(&'b self) -> impl ExactSizeIterator<Item = &'b Multiaddr> + 'b {
        self.peerset.peers[self.peer_index].addresses.iter()
    }

    /// Returns an iterator to the list of addresses known for this peer.
    ///
    /// Filters out addresses to which there is an ongoing connection attempt.
    pub fn known_addresses_no_pending<'b>(&'b self) -> impl Iterator<Item = &'b Multiaddr> + 'b {
        self.known_addresses().filter(move |addr| {
            !self
                .pending_connections()
                .any(move |idx| match &self.peerset.connections[idx.0].ty {
                    ConnectionTy::Pending { target, .. } => target == *addr,
                    _ => false,
                })
        })
    }

    /// Adds the node to an overlay network.
    ///
    /// Has no effect if this node is already in this overlay network.
    ///
    /// # Panic
    ///
    /// Panics if `overlay_network_index` is out of range.
    ///
    pub fn add_to_overlay(&mut self, overlay_network_index: usize) {
        assert!(overlay_network_index < self.peerset.num_overlay_networks);
        self.peerset
            .peers_overlays
            .insert((self.peer_index, overlay_network_index));
        self.peerset
            .overlay_peers
            .insert((overlay_network_index, self.peer_index));
    }

    /// Removes the node from an overlay network.
    ///
    /// Returns `true` if the node was indeed part of this overlay network.
    ///
    /// # Panic
    ///
    /// Panics if `overlay_network_index` is out of range.
    ///
    pub fn remove_from_overlay(&mut self, overlay_network_index: usize) -> bool {
        assert!(overlay_network_index < self.peerset.num_overlay_networks);
        let was_in1 = self
            .peerset
            .peers_overlays
            .remove(&(self.peer_index, overlay_network_index));
        let was_in2 = self
            .peerset
            .overlay_peers
            .remove(&(overlay_network_index, self.peer_index));
        debug_assert_eq!(was_in1, was_in2);
        was_in1
    }

    /// Gives access to the user data associated with the node.
    pub fn user_data_mut(&mut self) -> &mut TPeer {
        &mut self.peerset.peers[self.peer_index].user_data
    }

    /// Gives access to the user data associated with the node.
    pub fn into_user_data(self) -> &'a mut TPeer {
        &mut self.peerset.peers[self.peer_index].user_data
    }
}

/// Access to a node that isn't known to the data structure.
pub struct NodeMutUnknown<'a, TPeer, TConn, TPending> {
    peerset: &'a mut Peerset<TPeer, TConn, TPending>,
    peer_id: PeerId,
}

impl<'a, TPeer, TConn, TPending> NodeMutUnknown<'a, TPeer, TConn, TPending> {
    /// Inserts the node into the data structure. Returns a [`NodeMutKnown`] for that node.
    pub fn insert(self, user_data: TPeer) -> NodeMutKnown<'a, TPeer, TConn, TPending> {
        let peer_index = self.peerset.peers.insert(Peer {
            peer_id: self.peer_id.clone(),
            user_data,
            addresses: Vec::new(),
            connected: true,
        });

        let _was_in = self.peerset.peer_ids.insert(self.peer_id, peer_index);
        debug_assert!(_was_in.is_none());

        NodeMutKnown {
            peerset: self.peerset,
            peer_index,
        }
    }
}
